//! The one SQL reader of an item field, and the column and index that serve
//! it.
//!
//! Items keep every field in one JSONB column, `item.fields`, whose documents
//! are large (hundreds of keys on a big content type) and stored compressed
//! out of line: reading one key of one item decompresses the whole document.
//! A field marked "Filterable and sortable" is therefore also copied, as a
//! bare value, into a second, small JSONB column, `item.filter_values`, which
//! holds only the marked fields of the item's type and stays in the row
//! itself. A trigger keeps the copy current on every write
//! ([`trigger_function_sql`]); a background job fills it for existing items
//! when a field is marked, then builds the field's index and statistics
//! (`super::field_index`).
//!
//! PostgreSQL uses an index for a condition only when the condition's
//! expression is the index's expression. So every place that reads a field
//! in SQL (Gather's filters, sorts, exposed and contextual filters, facets,
//! and the built-in filter handlers) calls [`read`], and the index and
//! statistics for a field are built from the same expressions
//! ([`index_ddl`], [`stats_ddl`]). Nothing else writes `fields->...` or
//! `filter_values->...`, so they cannot drift.
//!
//! A field is read one of three ways, its [`FieldKind`]:
//!
//! - **Text**: the payload of a `{"value": …}` wrapper or the bare value, as
//!   text. Short text, dates (ISO strings sort and compare as text), booleans
//!   (`true`/`false`) and single references.
//! - **Number**: the same payload as `numeric` (`trovato_field_numeric`): a
//!   JSON number, or a string that spells one; anything else is NULL.
//!   Integers and decimals, so `fi_price >= 5000` compares numbers, not
//!   text, and never fails.
//! - **List**: the payload as `jsonb`, for a field that holds several values
//!   (tags, multi value references, any field with a cardinality other than
//!   one), matched with `@>` containment. Its index is a GIN on the
//!   elements as a `text[]` (`trovato_field_list`), with statistics on the
//!   most common elements, and a served field is matched there
//!   ([`list_contains`]).
//!
//! A field that is not marked, or whose copy is still being filled, is read
//! from `fields` with the same kind: the same answer, without an index.
//!
//! See `docs/design/field-indexes.md` for why a copy column with expression
//! indexes, and not expression indexes on `fields`, generated columns, a side
//! table or one GIN over the whole column.

use trovato_sdk::types::{FieldDefinition, FieldType};

use super::handlers::is_safe_identifier;

/// How a field is read in SQL, and so how it is indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// One value, compared as text.
    Text,
    /// One value, compared as a number.
    Number,
    /// Several values, matched by containment.
    List,
}

impl FieldKind {
    /// The kind a field is read as, or `None` for a field no index can serve:
    /// long text (a B-tree entry is limited to about 2.7 kB), and the
    /// structured kinds (blocks, page builder, compound), whose values are
    /// documents, not values.
    pub fn of(def: &FieldDefinition) -> Option<Self> {
        let several = def.cardinality != 1;
        let kind = match &def.field_type {
            FieldType::Integer | FieldType::Float => Self::Number,
            FieldType::CategoryReference(_) => Self::List,
            FieldType::Text { .. }
            | FieldType::Boolean
            | FieldType::Date
            | FieldType::Email
            | FieldType::File
            | FieldType::RecordReference(_) => Self::Text,
            _ => return None,
        };
        Some(if several { Self::List } else { kind })
    }

    /// The name stored in `item_field_index.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::List => "list",
        }
    }

    /// The kind stored under [`Self::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "list" => Some(Self::List),
            _ => None,
        }
    }
}

/// The column marked fields are copied into.
pub const COPY_COLUMN: &str = "filter_values";

/// The field value as text: a `{"value": …}` wrapper's payload, or the bare
/// value.
///
/// Mirrors `scalar_of` in `crates/kernel/src/content/field_value.rs` and the
/// `item_search_vector` SQL function: `column->key->'value'` is `NULL` on a
/// bare scalar, so `COALESCE` falls through to `column->>key`.
///
/// `column` is a JSONB column expression built from validated identifiers
/// (`item.fields`); `key` is a quoted, validated literal (`'city'`) or a bind
/// placeholder (`$1`). Only a literal key can match an index.
pub fn text_sql(column: &str, key: &str) -> String {
    format!("COALESCE({column}->{key}->>'value', {column}->>{key})")
}

/// The same payload as `jsonb`.
pub fn json_sql(column: &str, key: &str) -> String {
    format!("COALESCE({column}->{key}->'value', {column}->{key})")
}

/// The same payload as `numeric` (NULL when it is not a number).
pub fn number_sql(column: &str, key: &str) -> String {
    format!("trovato_field_numeric({})", json_sql(column, key))
}

/// The reader of `field` of `kind` on `table`'s `fields` column: the answer
/// without an index.
///
/// `table` and `field` must already have passed `is_safe_identifier`; this
/// interpolates them.
pub fn reader(table: &str, field: &str, kind: FieldKind) -> String {
    let column = format!("{table}.fields");
    let key = format!("'{field}'");
    match kind {
        FieldKind::Text => text_sql(&column, &key),
        FieldKind::Number => number_sql(&column, &key),
        FieldKind::List => json_sql(&column, &key),
    }
}

/// The reader of `field` of `kind` on `column`, a copy column expression
/// (`item.filter_values`, or the bare `filter_values` an index is built
/// on). The copy holds the bare value, already unwrapped and normalized by
/// [`copy_value_sql`], so the reader is a plain extraction.
fn copy_reader(column: &str, field: &str, kind: FieldKind) -> String {
    match kind {
        FieldKind::Text => format!("({column}->>'{field}')"),
        FieldKind::Number => format!("trovato_field_numeric({column}->'{field}')"),
        FieldKind::List => format!("({column}->'{field}')"),
    }
}

/// How Gather reads `field` of `kind` on `table`: from the copy column when
/// `served` (the field is marked and its copy and index are ready), from
/// `fields` otherwise. Both give the same answer.
///
/// `table` and `field` must already have passed `is_safe_identifier`.
pub fn read(table: &str, field: &str, kind: FieldKind, served: bool) -> String {
    if served {
        copy_reader(&format!("{table}.{COPY_COLUMN}"), field, kind)
    } else {
        reader(table, field, kind)
    }
}

/// The value of `field` the copy column holds, computed from the `fields`
/// column expression `column` (`NEW.fields` in the trigger, `fields` in the
/// fill): text as a JSON string, a number as a JSON number, a list as it is
/// stored. NULL (and so absent, after `jsonb_strip_nulls`) when the field is
/// empty or, for a number, not one.
pub fn copy_value_sql(column: &str, field: &str, kind: FieldKind) -> String {
    let key = format!("'{field}'");
    match kind {
        FieldKind::Text => format!("to_jsonb({})", text_sql(column, &key)),
        FieldKind::Number => format!("to_jsonb({})", number_sql(column, &key)),
        FieldKind::List => json_sql(column, &key),
    }
}

/// The whole copy for an item of a type whose marked fields are `fields`,
/// from the `fields` column expression `column`. `jsonb_build_object` takes
/// at most 100 arguments, so the object is built 40 fields at a time and the
/// parts joined with `||`. `NULL` when no field is marked.
pub fn copy_object_sql(column: &str, fields: &[(String, FieldKind)]) -> String {
    if fields.is_empty() {
        return "NULL::jsonb".to_string();
    }
    let parts: Vec<String> = fields
        .chunks(40)
        .map(|chunk| {
            let pairs: Vec<String> = chunk
                .iter()
                .map(|(name, kind)| format!("'{name}', {}", copy_value_sql(column, name, *kind)))
                .collect();
            format!("jsonb_build_object({})", pairs.join(", "))
        })
        .collect();
    format!("jsonb_strip_nulls({})", parts.join(" || "))
}

/// The trigger function that keeps `item.filter_values` current: on every
/// insert, and every update of `type` or `fields`, it recomputes the copy
/// from `fields` for the item's type. `types` lists each type with its
/// marked fields; every name must already have passed
/// `is_safe_identifier`. Regenerated whenever the marked fields change.
pub fn trigger_function_sql(types: &[(String, Vec<(String, FieldKind)>)]) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION trovato_item_filter_values() RETURNS trigger \
         LANGUAGE plpgsql AS $fn${}$fn$",
        trigger_body_sql(types)
    )
}

/// The body of [`trigger_function_sql`]'s function, as `pg_proc.prosrc`
/// holds it, so the kernel can tell whether the installed one is current.
pub fn trigger_body_sql(types: &[(String, Vec<(String, FieldKind)>)]) -> String {
    let mut arms = String::new();
    for (item_type, fields) in types {
        if fields.is_empty() {
            continue;
        }
        arms.push_str(&format!(
            "\n    WHEN '{item_type}' THEN {}",
            copy_object_sql("NEW.fields", fields)
        ));
    }
    let body = if arms.is_empty() {
        "NEW.filter_values := NULL;".to_string()
    } else {
        format!("NEW.filter_values := CASE NEW.type{arms}\n    ELSE NULL END;")
    };
    format!("\nBEGIN\n  {body}\n  RETURN NEW;\nEND\n")
}

/// A stable short name for `item_type` and `field` under `prefix`: readable
/// when it fits PostgreSQL's 63 byte limit and names one pair only, a hash
/// of both names otherwise.
///
/// The readable form joins the two with `__`, which a name may also hold:
/// type `a__b` with field `c` and type `a` with field `b__c` would both be
/// `item_fx_a__b__c`, and one field's build or drop would replace the
/// other's index. It is used only when the joint is the one `__` in it (no
/// `__` in either name, no `_` at the end of the type or the start of the
/// field), so it splits back into one pair; the hashed form has no `__`, so
/// it never equals a readable one.
fn object_name(prefix: &str, item_type: &str, field: &str) -> String {
    let readable = format!("{prefix}_{item_type}__{field}");
    let unambiguous = !item_type.contains("__")
        && !field.contains("__")
        && !item_type.ends_with('_')
        && !field.starts_with('_');
    if unambiguous && readable.len() <= 63 {
        return readable;
    }
    // FNV-1a: stable across builds and platforms, no dependency.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in item_type.bytes().chain([0u8]).chain(field.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}_{hash:016x}")
}

/// The name of the index on `field` of `item_type`.
pub fn index_name(item_type: &str, field: &str) -> String {
    object_name("item_fx", item_type, field)
}

/// The name of the statistics object on `field` of `item_type`.
pub fn stats_name(item_type: &str, field: &str) -> String {
    object_name("item_fs", item_type, field)
}

/// The name of `item_type`'s GIN index over the whole copy column.
pub fn type_index_name(item_type: &str) -> String {
    object_name("item_fv", item_type, "all")
}

/// `CREATE INDEX CONCURRENTLY` for `field` of `item_type`, partial on the
/// type, over the copy reader. `None` when a name is not a safe identifier.
///
/// A one value field gets a B-tree on `(reader, created)`: the reader serves
/// equality, `IN`, ranges and sorts on the field, and `created`, the order a
/// listing is in unless it says otherwise, lets "this value, newest first"
/// read its page straight off the index. A list field gets a GIN on its
/// elements as a `text[]` (`indexed_expr`), which serves `@>`.
pub fn index_ddl(item_type: &str, field: &str, kind: FieldKind) -> Option<String> {
    if !is_safe_identifier(item_type) || !is_safe_identifier(field) {
        return None;
    }
    let name = index_name(item_type, field);
    // The index reads the bare column, as `CREATE INDEX` requires; the
    // planner matches it to Gather's `item.filter_values` reader.
    let expr = indexed_expr(COPY_COLUMN, field, kind);
    let body = match kind {
        FieldKind::Text | FieldKind::Number => format!("(({expr}), created)"),
        FieldKind::List => format!("USING gin (({expr}))"),
    };
    Some(format!(
        "CREATE INDEX CONCURRENTLY {name} ON item {body} WHERE type = '{item_type}'"
    ))
}

/// The expression a field's index and statistics are on, over a copy column
/// expression: the copy reader, except for a list field, whose elements are
/// indexed as a `text[]` (`trovato_field_list`). PostgreSQL keeps statistics
/// on the most common elements of an array, and estimates `@>` from them; of
/// a jsonb array it knows only whole arrays, and misjudges a tag.
fn indexed_expr(column: &str, field: &str, kind: FieldKind) -> String {
    match kind {
        FieldKind::List => format!("trovato_field_list({column}->'{field}')"),
        _ => copy_reader(column, field, kind),
    }
}

/// Whether a list field on `table` contains the value of `placeholder`
/// (`$1`): on the indexed `text[]` form of the copy when `served`, on the
/// same `text[]` form of the reader of `fields` otherwise.
///
/// Both compare elements as text, so marking a field never changes which
/// items match: a list of numbers `[7]` contains the value `7` either way.
/// (Containment on the jsonb array would not, as `[7]` does not contain
/// `["7"]`.)
///
/// `table` and `field` must already have passed `is_safe_identifier`.
pub fn list_contains(table: &str, field: &str, served: bool, placeholder: &str) -> String {
    format!(
        "{} @> array_append('{{}}'::text[], {placeholder}::text)",
        list_elements(table, field, served)
    )
}

/// The `text[]` of a list field's elements on `table`: from the copy when
/// `served`, from `fields` otherwise.
fn list_elements(table: &str, field: &str, served: bool) -> String {
    if served {
        indexed_expr(&format!("{table}.{COPY_COLUMN}"), field, FieldKind::List)
    } else {
        format!(
            "trovato_field_list({})",
            reader(table, field, FieldKind::List)
        )
    }
}

/// Whether a list field on `table` contains any of the values of
/// `placeholders` (`$1`, `$2`, …): one overlap (`&&`) on the `text[]` of
/// [`list_contains`], which, when `served`, the planner estimates from the
/// indexed form's element statistics as one condition. `None` for no
/// placeholders.
///
/// The array is built with `array_append`, not `ARRAY[…]`: the query
/// builder's placeholder parser does not see a `$1` inside brackets.
///
/// `table` and `field` must already have passed `is_safe_identifier`.
pub fn list_overlaps(
    table: &str,
    field: &str,
    served: bool,
    placeholders: &[String],
) -> Option<String> {
    if placeholders.is_empty() {
        return None;
    }
    let array = placeholders
        .iter()
        .fold("'{}'::text[]".to_string(), |acc, p| {
            format!("array_append({acc}, {p}::text)")
        });
    Some(format!(
        "{} && {array}",
        list_elements(table, field, served)
    ))
}

/// `CREATE STATISTICS` on the copy reader of `field`.
///
/// The planner estimates how many rows a condition on an expression matches
/// from statistics on that expression. `ANALYZE` gathers them for an
/// expression index, but the planner ignores those of a partial index, and
/// every field index is partial on its type; without these, it guesses, and
/// at a million items a guess picks plans a hundred times slower.
pub fn stats_ddl(item_type: &str, field: &str, kind: FieldKind) -> Option<String> {
    if !is_safe_identifier(item_type) || !is_safe_identifier(field) {
        return None;
    }
    Some(format!(
        "CREATE STATISTICS IF NOT EXISTS {} ON ({}) FROM item",
        stats_name(item_type, field),
        indexed_expr(COPY_COLUMN, field, kind)
    ))
}

/// `CREATE INDEX CONCURRENTLY` for `item_type`'s GIN over the whole copy
/// column, which answers several equalities at once (see
/// `GatherQueryBuilder`'s use of [`containment_sql`]).
pub fn type_index_ddl(item_type: &str) -> Option<String> {
    if !is_safe_identifier(item_type) {
        return None;
    }
    Some(format!(
        "CREATE INDEX CONCURRENTLY {} ON item USING gin ({COPY_COLUMN} jsonb_path_ops) \
         WHERE type = '{item_type}'",
        type_index_name(item_type)
    ))
}

/// The name of `item_type`'s index on `created`.
pub fn type_created_index_name(item_type: &str) -> String {
    object_name("item_fc", item_type, "created")
}

/// `CREATE INDEX CONCURRENTLY` for `item_type`'s index on `created`, partial
/// on the type. `idx_item_created` covers every type, so a listing of one
/// type, newest first, with a condition no field index orders (a tag, an
/// `OR`, a field not marked) visits every other type's items too; at half a
/// million items of other types around a common tag, most of its work.
pub fn type_created_index_ddl(item_type: &str) -> Option<String> {
    if !is_safe_identifier(item_type) {
        return None;
    }
    Some(format!(
        "CREATE INDEX CONCURRENTLY {} ON item (created) WHERE type = '{item_type}'",
        type_created_index_name(item_type)
    ))
}

/// Containment of every `(field, value placeholder)` pair in the copy
/// column on `table`: `item.filter_values @> jsonb_build_object('a', $1::text,
/// 'b', $2::text)`. For text fields only, whose copies are always strings.
pub fn containment_sql(table: &str, pairs: &[(String, String)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(field, value)| format!("'{field}', {value}"))
        .collect();
    format!(
        "{table}.{COPY_COLUMN} @> jsonb_build_object({})",
        body.join(", ")
    )
}

/// `DROP INDEX CONCURRENTLY` for the index named `name`.
pub fn drop_index_ddl(name: &str) -> Option<String> {
    is_safe_identifier(name).then(|| format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"))
}

/// `DROP STATISTICS` for `field` of `item_type`.
pub fn drop_stats_ddl(item_type: &str, field: &str) -> Option<String> {
    if !is_safe_identifier(item_type) || !is_safe_identifier(field) {
        return None;
    }
    Some(format!(
        "DROP STATISTICS IF EXISTS {}",
        stats_name(item_type, field)
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn readers_are_the_wrapped_value_readers() {
        assert_eq!(
            reader("item", "city", FieldKind::Text),
            "COALESCE(item.fields->'city'->>'value', item.fields->>'city')"
        );
        assert_eq!(
            reader("item", "price", FieldKind::Number),
            "trovato_field_numeric(COALESCE(item.fields->'price'->'value', item.fields->'price'))"
        );
        assert_eq!(
            reader("item", "tags", FieldKind::List),
            "COALESCE(item.fields->'tags'->'value', item.fields->'tags')"
        );
    }

    #[test]
    fn served_fields_read_the_copy() {
        assert_eq!(
            read("item", "city", FieldKind::Text, true),
            "(item.filter_values->>'city')"
        );
        assert_eq!(
            read("item", "price", FieldKind::Number, true),
            "trovato_field_numeric(item.filter_values->'price')"
        );
        assert_eq!(
            read("item", "city", FieldKind::Text, false),
            reader("item", "city", FieldKind::Text)
        );
    }

    #[test]
    fn index_and_statistics_read_the_same_expression() {
        let ddl = index_ddl("blog", "city", FieldKind::Text).unwrap();
        assert!(ddl.contains("((filter_values->>'city'))"), "{ddl}");
        assert!(ddl.ends_with("WHERE type = 'blog'"), "{ddl}");
        let ddl = index_ddl("blog", "tags", FieldKind::List).unwrap();
        assert!(
            ddl.contains("USING gin ((trovato_field_list(filter_values->'tags')))"),
            "{ddl}"
        );
        assert_eq!(
            list_contains("item", "tags", true, "$2"),
            "trovato_field_list(item.filter_values->'tags') @> array_append('{}'::text[], $2::text)"
        );
        assert_eq!(
            list_contains("item", "tags", false, "$1"),
            "trovato_field_list(COALESCE(item.fields->'tags'->'value', item.fields->'tags')) \
             @> array_append('{}'::text[], $1::text)"
        );
        let two = ["$1".to_string(), "$2".to_string()];
        assert_eq!(
            list_overlaps("item", "tags", true, &two).unwrap(),
            "trovato_field_list(item.filter_values->'tags') && \
             array_append(array_append('{}'::text[], $1::text), $2::text)"
        );
        let any = list_overlaps("item", "tags", false, &two).unwrap();
        assert!(
            any.starts_with("trovato_field_list(COALESCE(item.fields"),
            "{any}"
        );
        assert!(any.contains(" && "), "{any}");
        assert!(list_overlaps("item", "tags", true, &[]).is_none());
        let stats = stats_ddl("blog", "price", FieldKind::Number).unwrap();
        assert!(
            stats.contains("(trovato_field_numeric(filter_values->'price'))"),
            "{stats}"
        );
        assert!(index_ddl("blog", "x'; drop table item; --", FieldKind::Text).is_none());
        assert!(stats_ddl("blog", "x y", FieldKind::Text).is_none());
    }

    #[test]
    fn the_copy_is_built_in_parts_of_forty() {
        let fields: Vec<(String, FieldKind)> = (0..90)
            .map(|i| (format!("f{i}"), FieldKind::Text))
            .collect();
        let sql = copy_object_sql("NEW.fields", &fields);
        assert_eq!(sql.matches("jsonb_build_object(").count(), 3);
        assert!(sql.starts_with("jsonb_strip_nulls("));
        assert_eq!(copy_object_sql("fields", &[]), "NULL::jsonb");
    }

    #[test]
    fn the_trigger_has_one_arm_per_type_with_marked_fields() {
        let sql = trigger_function_sql(&[
            ("blog".into(), vec![("city".into(), FieldKind::Text)]),
            ("page".into(), vec![]),
        ]);
        assert!(sql.contains("WHEN 'blog' THEN"), "{sql}");
        assert!(!sql.contains("'page'"), "{sql}");
        let empty = trigger_function_sql(&[]);
        assert!(empty.contains("NEW.filter_values := NULL;"), "{empty}");
    }

    #[test]
    fn long_names_hash() {
        let name = index_name(&"t".repeat(32), &"f".repeat(40));
        assert!(name.len() <= 63);
        assert_eq!(name, index_name(&"t".repeat(32), &"f".repeat(40)));
        assert_eq!(index_name("blog", "city"), "item_fx_blog__city");
        assert_eq!(stats_name("blog", "city"), "item_fs_blog__city");
        assert_eq!(type_index_name("blog"), "item_fv_blog__all");
        assert_eq!(type_created_index_name("blog"), "item_fc_blog__created");
        assert_eq!(
            type_created_index_ddl("blog").unwrap(),
            "CREATE INDEX CONCURRENTLY item_fc_blog__created ON item (created) WHERE type = 'blog'"
        );
        assert!(type_created_index_ddl("bl'og").is_none());
    }

    #[test]
    fn names_that_could_split_two_ways_hash() {
        // Type `a__b` with field `c` and type `a` with field `b__c` read
        // alike, as do `a_` + `b` and `a` + `_b`.
        let pairs = [
            ("a__b", "c"),
            ("a", "b__c"),
            ("a_", "_b"),
            ("a_", "b"),
            ("a", "_b"),
        ];
        let names: Vec<String> = pairs.iter().map(|(t, f)| index_name(t, f)).collect();
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
            assert!(!a.contains("__"), "{a}");
            assert_eq!(a, &index_name(pairs[i].0, pairs[i].1));
        }
        assert_ne!(stats_name("a__b", "c"), stats_name("a", "b__c"));
        // A single underscore inside either name keeps the readable form.
        assert_eq!(
            index_name("blog_post", "sub_title"),
            "item_fx_blog_post__sub_title"
        );
    }
}

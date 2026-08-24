//! Book-style page trees: ordered hierarchy with previous/next/up navigation.
//!
//! Nothing in Trovato provided the Drupal 6 book model, and menu hierarchy alone does
//! not give it: a menu answers "what is under this", and a book answers "what comes
//! next", which is a different question and needs a total order over the whole tree.
//! The docs site wants this immediately — nine tutorial parts and nineteen design
//! documents are two books.
//!
//! # Storage
//!
//! One plugin-owned table, `book_page`, declared in `[capabilities] db_tables`. A row
//! per page: the item, its book, its parent, its weight. The kernel's tables are
//! untouched — a `book_id` column on `item` would make every site carry a column for
//! a feature most of them do not use, and would put a plugin's schema inside the
//! kernel's.
//!
//! A **book is identified by its own root page** (`book_id = item_id` for the root)
//! rather than by a separate book entity. That keeps the model to one table and makes
//! "which book is this page in" a column read rather than a join, and it means a book
//! is created by declaring its first page a root.
//!
//! # Reading order
//!
//! Depth-first, siblings by `(weight, title)`. That is the order a reader moves
//! through, so prev/next is "the page before/after this one in that sequence" and
//! traverses the whole book exactly once. Ties break on title so the order is total:
//! two siblings at the same weight must still have a defined "next".
//!
//! # What this plugin cannot do on the 0.102 contract
//!
//! Two things the design would want, both blocked on kernel seams that do not exist.
//! Recorded here rather than worked around, because a reader of this file is the
//! person who will next want them:
//!
//! 1. **A fieldset on the item form.** The item form does not go through
//!    `FormService`, so `tap_form_alter`, `tap_form_validate` and `tap_form_submit`
//!    are never dispatched for it — `FormService` is constructed and exposed on
//!    `AppState` and no route calls `build` or `process`. Authoring therefore lives on
//!    this plugin's own screens, under `/admin/structure/books`, which is a complete
//!    path and not the one a Drupal user would expect.
//! 2. **A sidebar tile rendering the tree.** `services/tile.rs` dispatches on
//!    `tile_type` in a closed `match` in the kernel, so a plugin cannot register a
//!    tile type. The tree is rendered into the item view instead, which puts it in the
//!    content region rather than the sidebar.
//!
//! Both are additive kernel changes rather than contract breaks, and both are larger
//! than this plugin.
//!
//! # What it does do
//!
//! `tap_item_view` decorates a page with its breadcrumb trail, its up link and its
//! previous/next links, and with the book's tree. `tap_menu` plus `tap_api` serve the
//! admin screens. Placement, reparenting, reordering and removal happen there, with
//! cycle rejection on save.

use std::collections::HashMap;

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{ApiRequest, ApiResponse, MenuRoute};

/// Plugin name, for logging.
const PLUGIN_NAME: &str = "trovato_book";

/// The permission the admin screens are gated on.
const PERM_ADMINISTER: &str = "administer books";

/// How deep the tree renders before it stops.
///
/// A guard, not a design limit: cycles are rejected on save, and this keeps a row
/// that predates that rejection from rendering forever.
const MAX_DEPTH: usize = 32;

// ─── Permissions ─────────────────────────────────────────────────────

/// Declare the permission the admin screens gate on.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM_ADMINISTER,
        "Create books and place, reorder and remove their pages",
    )]
}

// ─── Routes ──────────────────────────────────────────────────────────

/// Register the admin screens.
///
/// `MenuRoute::api` rather than `MenuDefinition`, because a `MenuDefinition` leaves
/// `handler_type` at `"page"` and the kernel routes to `tap_api` only for an `"api"`
/// entry — declaring a callback on a page entry produces a path that is registered,
/// gated, listed and 404.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("GET", "/admin/structure/books", "book_list")
            .title("Books")
            .permission(PERM_ADMINISTER)
            .parent("/admin/structure")
            .visible(),
        MenuRoute::api("GET", "/admin/structure/books/:book", "book_tree")
            .title("Book")
            .permission(PERM_ADMINISTER),
        MenuRoute::api("POST", "/admin/structure/books/create", "book_create")
            .title("Create a book")
            .permission(PERM_ADMINISTER),
        MenuRoute::api("POST", "/admin/structure/books/:book/place", "book_place")
            .title("Place a page")
            .permission(PERM_ADMINISTER),
        MenuRoute::api("POST", "/admin/structure/books/:book/remove", "book_remove")
            .title("Remove a page")
            .permission(PERM_ADMINISTER),
    ]
}

/// Serve one admin request.
#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "book_list" => book_list(),
        "book_tree" => book_tree(&request),
        "book_create" => book_create(&request),
        "book_place" => book_place(&request),
        "book_remove" => book_remove(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

// ─── The page decoration ─────────────────────────────────────────────

/// Decorate a page that belongs to a book.
///
/// Returns an empty string for an item that is not in one, which is most items on
/// most sites, and is the one case worth being cheap: it is a single indexed read.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    // The item's id as a string, which is what this plugin's rows carry.
    let item_id = item.id.to_string();
    let Some(page) = load_page(&item_id) else {
        return String::new();
    };
    let pages = load_book(&page.book_id);
    if pages.is_empty() {
        return String::new();
    }

    let order = reading_order(&pages);
    let position = order.iter().position(|p| p.item_id == item_id);

    let mut html = String::from("<nav class=\"book-nav\" aria-label=\"Book\">\n");

    // The trail from the book's root down to this page's parent.
    let trail = ancestors_of(&pages, &item_id);
    if !trail.is_empty() {
        html.push_str("<ol class=\"book-nav__trail\">\n");
        for ancestor in &trail {
            html.push_str(&format!(
                "<li><a href=\"/item/{}\">{}</a></li>\n",
                escape_html(&ancestor.item_id),
                escape_html(&ancestor.title)
            ));
        }
        html.push_str("</ol>\n");
    }

    if let Some(index) = position {
        html.push_str("<ul class=\"book-nav__links\">\n");
        if let Some(previous) = order.get(index.wrapping_sub(1)).filter(|_| index > 0) {
            html.push_str(&format!(
                "<li class=\"book-nav__prev\"><a rel=\"prev\" href=\"/item/{}\">&larr; {}</a></li>\n",
                escape_html(&previous.item_id),
                escape_html(&previous.title)
            ));
        }
        if let Some(parent) = trail.last() {
            html.push_str(&format!(
                "<li class=\"book-nav__up\"><a rel=\"up\" href=\"/item/{}\">&uarr; {}</a></li>\n",
                escape_html(&parent.item_id),
                escape_html(&parent.title)
            ));
        }
        if let Some(next) = order.get(index + 1) {
            html.push_str(&format!(
                "<li class=\"book-nav__next\"><a rel=\"next\" href=\"/item/{}\">{} &rarr;</a></li>\n",
                escape_html(&next.item_id),
                escape_html(&next.title)
            ));
        }
        html.push_str("</ul>\n");
    }

    // The whole tree, with this page marked. This is where the sidebar tile would
    // have gone; see the module docs for why it is here instead.
    html.push_str("<div class=\"book-nav__tree\">\n");
    html.push_str(&render_tree(&pages, Some(&item_id)));
    html.push_str("</div>\n</nav>\n");

    html
}

// ─── Admin screens ───────────────────────────────────────────────────

/// Every book, with its page count.
fn book_list() -> ApiResponse {
    let books = load_books();

    let mut body = String::new();
    if books.is_empty() {
        body.push_str(
            "<p>No books yet. A book is created by declaring an existing item its root \
             page.</p>\n",
        );
    } else {
        body.push_str("<table>\n<thead><tr><th>Book</th><th>Pages</th></tr></thead>\n<tbody>\n");
        for book in &books {
            body.push_str(&format!(
                "<tr><td><a href=\"/admin/structure/books/{id}\">{title}</a></td>\
                 <td>{pages}</td></tr>\n",
                id = escape_html(&book.book_id),
                title = escape_html(&book.title),
                pages = book.pages
            ));
        }
        body.push_str("</tbody>\n</table>\n");
    }

    body.push_str(
        "<h2>Create a book</h2>\n\
         <form method=\"post\" action=\"/admin/structure/books/create\">\n\
         <label for=\"item_id\">Root page item id</label>\n\
         <input type=\"text\" id=\"item_id\" name=\"item_id\" required>\n\
         <button type=\"submit\">Create book</button>\n\
         </form>\n\
         <p>The item becomes the book's first page. Any item type can be a page.</p>\n",
    );

    admin_page("Books", &body)
}

/// One book as an ordered tree, with the forms that edit it.
fn book_tree(request: &ApiRequest) -> ApiResponse {
    let Some(book_id) = request.params.get("book") else {
        return ApiResponse::error(400, "missing book id");
    };
    let pages = load_book(book_id);
    if pages.is_empty() {
        return ApiResponse::error(404, "no such book");
    }

    let order = reading_order(&pages);
    let mut body = String::from("<p>Reading order, depth first by weight then title.</p>\n");
    body.push_str(
        "<table>\n<thead><tr><th>#</th><th>Page</th><th>Depth</th><th>Weight</th>\
                   <th></th></tr></thead>\n<tbody>\n",
    );
    for (index, page) in order.iter().enumerate() {
        body.push_str(&format!(
            "<tr><td>{n}</td><td>{indent}<a href=\"/item/{id}\">{title}</a></td>\
             <td>{depth}</td><td>{weight}</td><td>\
             <form method=\"post\" action=\"/admin/structure/books/{book}/remove\">\
             <input type=\"hidden\" name=\"item_id\" value=\"{id}\">\
             <button type=\"submit\">Remove</button></form></td></tr>\n",
            n = index + 1,
            indent = "&mdash; ".repeat(page.depth),
            id = escape_html(&page.item_id),
            title = escape_html(&page.title),
            depth = page.depth,
            weight = page.weight,
            book = escape_html(book_id),
        ));
    }
    body.push_str("</tbody>\n</table>\n");

    body.push_str(&format!(
        "<h2>Place a page</h2>\n\
         <form method=\"post\" action=\"/admin/structure/books/{book}/place\">\n\
         <label for=\"item_id\">Item id</label>\n\
         <input type=\"text\" id=\"item_id\" name=\"item_id\" required>\n\
         <label for=\"parent_item_id\">Parent page item id (blank for top level)</label>\n\
         <input type=\"text\" id=\"parent_item_id\" name=\"parent_item_id\">\n\
         <label for=\"weight\">Weight</label>\n\
         <input type=\"number\" id=\"weight\" name=\"weight\" value=\"0\">\n\
         <button type=\"submit\">Place</button>\n\
         </form>\n\
         <p>Placing a page that is already in this book moves it. A page cannot be \
         placed under itself or under one of its own descendants.</p>\n",
        book = escape_html(book_id),
    ));

    admin_page("Book", &body)
}

/// Declare an item the root of a new book.
fn book_create(request: &ApiRequest) -> ApiResponse {
    let Some(item_id) = form_field(&request.body, "item_id") else {
        return ApiResponse::error(400, "missing item_id");
    };
    if !item_exists(&item_id) {
        return ApiResponse::error(400, "no such item");
    }
    if load_page(&item_id).is_some() {
        return ApiResponse::error(
            400,
            "that item is already a page in a book; an item belongs to at most one",
        );
    }

    // A root is its own book: `book_id = item_id`, no parent.
    let written = host::execute_raw(
        "INSERT INTO book_page (item_id, book_id, parent_item_id, weight) \
         VALUES ($1::uuid, $1::uuid, NULL, 0)",
        &[serde_json::json!(item_id)],
    );
    match written {
        Ok(_) => redirect(&format!("/admin/structure/books/{item_id}")),
        Err(code) => {
            host::log(PLUGIN_NAME, "error", &format!("book create failed: {code}"));
            ApiResponse::error(500, "could not create the book")
        }
    }
}

/// Place or move a page within a book.
fn book_place(request: &ApiRequest) -> ApiResponse {
    let Some(book_id) = request.params.get("book").cloned() else {
        return ApiResponse::error(400, "missing book id");
    };
    let Some(item_id) = form_field(&request.body, "item_id") else {
        return ApiResponse::error(400, "missing item_id");
    };
    let parent = form_field(&request.body, "parent_item_id").filter(|p| !p.is_empty());
    let weight: i64 = form_field(&request.body, "weight")
        .and_then(|w| w.parse().ok())
        .unwrap_or(0);

    if !item_exists(&item_id) {
        return ApiResponse::error(400, "no such item");
    }
    if load_book(&book_id).is_empty() {
        return ApiResponse::error(404, "no such book");
    }

    // An item belongs to at most one book. Moving within this one is fine; moving in
    // from another is not, because the other book's readers would lose a page
    // silently.
    if let Some(existing) = load_page(&item_id)
        && existing.book_id != book_id
    {
        return ApiResponse::error(
            400,
            "that item is already a page in another book; remove it from that one first",
        );
    }

    if let Some(parent_id) = parent.as_deref() {
        let Some(parent_page) = load_page(parent_id) else {
            return ApiResponse::error(400, "the parent is not a page in any book");
        };
        if parent_page.book_id != book_id {
            return ApiResponse::error(400, "the parent must be a page in this book");
        }
        // The whole reason placement is validated here rather than by a constraint:
        // a self-referential foreign key permits a cycle, and a cyclic book is one
        // that cannot be read.
        let pages = load_book(&book_id);
        if would_cycle(&pages, &item_id, parent_id) {
            return ApiResponse::error(
                400,
                "a page cannot be placed under itself or under one of its own descendants",
            );
        }
    }

    let written = host::execute_raw(
        "INSERT INTO book_page (item_id, book_id, parent_item_id, weight) \
         VALUES ($1::uuid, $2::uuid, $3::uuid, $4) \
         ON CONFLICT (item_id) DO UPDATE SET \
         book_id = EXCLUDED.book_id, parent_item_id = EXCLUDED.parent_item_id, \
         weight = EXCLUDED.weight",
        &[
            serde_json::json!(item_id),
            serde_json::json!(book_id),
            serde_json::json!(parent),
            serde_json::json!(weight),
        ],
    );
    match written {
        Ok(_) => redirect(&format!("/admin/structure/books/{book_id}")),
        Err(code) => {
            host::log(PLUGIN_NAME, "error", &format!("book place failed: {code}"));
            ApiResponse::error(500, "could not place the page")
        }
    }
}

/// Remove a page, promoting its children to its own parent.
fn book_remove(request: &ApiRequest) -> ApiResponse {
    let Some(book_id) = request.params.get("book").cloned() else {
        return ApiResponse::error(400, "missing book id");
    };
    let Some(item_id) = form_field(&request.body, "item_id") else {
        return ApiResponse::error(400, "missing item_id");
    };
    let Some(page) = load_page(&item_id) else {
        return ApiResponse::error(404, "that item is not a page in a book");
    };
    if page.book_id != book_id {
        return ApiResponse::error(400, "that page is not in this book");
    }

    // Removing a root removes the book: every page's `book_id` points at it, so the
    // rest would be orphaned rather than reparented.
    if page.parent_item_id.is_none() {
        if let Err(code) = host::execute_raw(
            "DELETE FROM book_page WHERE book_id = $1::uuid",
            &[serde_json::json!(book_id)],
        ) {
            host::log(PLUGIN_NAME, "error", &format!("book delete failed: {code}"));
            return ApiResponse::error(500, "could not remove the book");
        }
        return redirect("/admin/structure/books");
    }

    // Promote the children to the removed page's own parent, so a branch does not
    // become a row of top-level pages.
    if let Err(code) = host::execute_raw(
        "UPDATE book_page SET parent_item_id = $1::uuid WHERE parent_item_id = $2::uuid",
        &[
            serde_json::json!(page.parent_item_id),
            serde_json::json!(item_id),
        ],
    ) {
        host::log(PLUGIN_NAME, "error", &format!("promote failed: {code}"));
        return ApiResponse::error(500, "could not reparent the page's children");
    }

    match host::execute_raw(
        "DELETE FROM book_page WHERE item_id = $1::uuid",
        &[serde_json::json!(item_id)],
    ) {
        Ok(_) => redirect(&format!("/admin/structure/books/{book_id}")),
        Err(code) => {
            host::log(PLUGIN_NAME, "error", &format!("book remove failed: {code}"));
            ApiResponse::error(500, "could not remove the page")
        }
    }
}

// ─── The model ───────────────────────────────────────────────────────

/// One page, as the tree functions see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The item that is this page.
    pub item_id: String,
    /// The book's root item id.
    pub book_id: String,
    /// The parent page, or `None` for a root.
    pub parent_item_id: Option<String>,
    /// Sort weight among siblings.
    pub weight: i64,
    /// The item's title, for display and for breaking weight ties.
    pub title: String,
    /// Depth from the root, filled in by [`reading_order`].
    pub depth: usize,
}

/// A book on the listing.
struct BookSummary {
    book_id: String,
    title: String,
    pages: usize,
}

/// The reading order: depth first, siblings by (weight, title).
///
/// Total by construction, which is what makes "the next page" answerable: two
/// siblings at the same weight still have a defined order. Every page is emitted
/// exactly once, and a page whose parent is missing from the set is treated as a
/// root rather than dropped — a page that cannot be reached is exactly the page an
/// operator needs to see in order to fix it.
pub fn reading_order(pages: &[Page]) -> Vec<Page> {
    let ids: std::collections::HashSet<&str> = pages.iter().map(|p| p.item_id.as_str()).collect();

    let mut children: HashMap<Option<&str>, Vec<&Page>> = HashMap::new();
    for page in pages {
        let parent = match page.parent_item_id.as_deref() {
            Some(parent) if ids.contains(parent) => Some(parent),
            // Absent, or pointing outside this book: a root.
            _ => None,
        };
        children.entry(parent).or_default().push(page);
    }
    for group in children.values_mut() {
        group.sort_by(|a, b| a.weight.cmp(&b.weight).then_with(|| a.title.cmp(&b.title)));
    }

    let mut ordered = Vec::with_capacity(pages.len());
    let mut emitted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut stack: Vec<(Option<&str>, usize)> = vec![(None, 0)];

    // An explicit stack rather than recursion: the depth guard is then a property of
    // the loop rather than of the call depth.
    while let Some((parent, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Some(group) = children.get(&parent) else {
            continue;
        };
        // Reversed, because the stack pops in reverse.
        for page in group.iter().rev() {
            stack.push((Some(page.item_id.as_str()), depth + 1));
        }
        // Emit in order by walking the group forward, and push children after, so a
        // parent precedes its subtree.
        let _ = group;
        for page in group.iter() {
            if emitted.insert(page.item_id.as_str()) {
                let mut page = (*page).clone();
                page.depth = depth;
                ordered.push(page);
            }
        }
    }

    // The stack above emits breadth-first within a level; re-walk depth-first for the
    // reading order, using the ordering already established.
    let mut result = Vec::with_capacity(pages.len());
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    walk(&children, None, 0, &mut visited, &mut result);

    // Anything unreachable (a pre-existing cycle) is appended rather than lost.
    for page in pages {
        if !visited.contains(page.item_id.as_str()) {
            let mut page = page.clone();
            page.depth = 0;
            result.push(page);
        }
    }

    result
}

/// Depth-first walk, parent before subtree.
fn walk<'a>(
    children: &HashMap<Option<&'a str>, Vec<&'a Page>>,
    parent: Option<&'a str>,
    depth: usize,
    visited: &mut std::collections::HashSet<&'a str>,
    out: &mut Vec<Page>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(group) = children.get(&parent) else {
        return;
    };
    for page in group {
        if !visited.insert(page.item_id.as_str()) {
            continue;
        }
        let mut owned = (*page).clone();
        owned.depth = depth;
        out.push(owned);
        walk(
            children,
            Some(page.item_id.as_str()),
            depth + 1,
            visited,
            out,
        );
    }
}

/// The ancestors of a page, root first, excluding the page itself.
pub fn ancestors_of(pages: &[Page], item_id: &str) -> Vec<Page> {
    let by_id: HashMap<&str, &Page> = pages.iter().map(|p| (p.item_id.as_str(), p)).collect();
    let mut trail = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    seen.insert(item_id);

    let mut cursor = by_id.get(item_id).and_then(|p| p.parent_item_id.as_deref());
    while let Some(id) = cursor {
        if !seen.insert(id) {
            // A cycle. Stop rather than loop; the tree view shows the page anyway.
            break;
        }
        let Some(page) = by_id.get(id) else { break };
        trail.push((*page).clone());
        cursor = page.parent_item_id.as_deref();
        if trail.len() > MAX_DEPTH {
            break;
        }
    }
    trail.reverse();
    trail
}

/// Whether placing `item_id` under `parent_id` would create a cycle.
///
/// True when the parent is the page itself, or is one of its descendants. Walks
/// upward from the proposed parent, which terminates because the walk stops on a
/// repeat.
pub fn would_cycle(pages: &[Page], item_id: &str, parent_id: &str) -> bool {
    if item_id == parent_id {
        return true;
    }
    let by_id: HashMap<&str, &Page> = pages.iter().map(|p| (p.item_id.as_str(), p)).collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut cursor = Some(parent_id);
    while let Some(id) = cursor {
        if id == item_id {
            return true;
        }
        if !seen.insert(id) {
            // A cycle that already exists. Refusing is right either way.
            return true;
        }
        cursor = by_id.get(id).and_then(|p| p.parent_item_id.as_deref());
    }
    false
}

/// Render the tree as a nested list, marking `current`.
pub fn render_tree(pages: &[Page], current: Option<&str>) -> String {
    let order = reading_order(pages);
    if order.is_empty() {
        return String::new();
    }

    let mut html = String::new();
    let mut open = 0usize;
    for page in &order {
        while open > page.depth {
            html.push_str("</ul>\n");
            open -= 1;
        }
        while open < page.depth + 1 {
            html.push_str("<ul class=\"book-tree\">\n");
            open += 1;
        }
        let is_current = current == Some(page.item_id.as_str());
        html.push_str(&format!(
            "<li{cls}><a href=\"/item/{id}\"{aria}>{title}</a></li>\n",
            cls = if is_current {
                " class=\"book-tree__current\""
            } else {
                ""
            },
            aria = if is_current {
                " aria-current=\"page\""
            } else {
                ""
            },
            id = escape_html(&page.item_id),
            title = escape_html(&page.title),
        ));
    }
    while open > 0 {
        html.push_str("</ul>\n");
        open -= 1;
    }
    html
}

// ─── Database ────────────────────────────────────────────────────────

fn rows(sql: &str, params: &[serde_json::Value]) -> Vec<serde_json::Value> {
    match host::query_raw(sql, params) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(code) => {
            host::log(PLUGIN_NAME, "error", &format!("query failed: {code}"));
            Vec::new()
        }
    }
}

fn page_from_row(row: &serde_json::Value) -> Option<Page> {
    Some(Page {
        item_id: row.get("item_id")?.as_str()?.to_string(),
        book_id: row.get("book_id")?.as_str()?.to_string(),
        parent_item_id: row
            .get("parent_item_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        weight: row
            .get("weight")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: row
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(untitled)")
            .to_string(),
        depth: 0,
    })
}

/// One page, or `None` when the item is not in a book.
fn load_page(item_id: &str) -> Option<Page> {
    let rows = rows(
        "SELECT bp.item_id, bp.book_id, bp.parent_item_id, bp.weight, i.title \
         FROM book_page bp JOIN item i ON i.id = bp.item_id \
         WHERE bp.item_id = $1::uuid",
        &[serde_json::json!(item_id)],
    );
    rows.first().and_then(page_from_row)
}

/// Every page of one book.
fn load_book(book_id: &str) -> Vec<Page> {
    rows(
        "SELECT bp.item_id, bp.book_id, bp.parent_item_id, bp.weight, i.title \
         FROM book_page bp JOIN item i ON i.id = bp.item_id \
         WHERE bp.book_id = $1::uuid ORDER BY bp.weight, i.title",
        &[serde_json::json!(book_id)],
    )
    .iter()
    .filter_map(page_from_row)
    .collect()
}

/// Every book, with its page count.
fn load_books() -> Vec<BookSummary> {
    rows(
        "SELECT bp.book_id, i.title, COUNT(*) AS pages \
         FROM book_page bp JOIN item i ON i.id = bp.book_id \
         GROUP BY bp.book_id, i.title ORDER BY i.title",
        &[],
    )
    .iter()
    .filter_map(|row| {
        Some(BookSummary {
            book_id: row.get("book_id")?.as_str()?.to_string(),
            title: row
                .get("title")?
                .as_str()
                .unwrap_or("(untitled)")
                .to_string(),
            pages: usize::try_from(row.get("pages").and_then(serde_json::Value::as_i64)?).ok()?,
        })
    })
    .collect()
}

/// Whether an item exists. A page has to be an item; a typo should say so rather
/// than creating a page pointing at nothing.
fn item_exists(item_id: &str) -> bool {
    !rows(
        "SELECT id FROM item WHERE id = $1::uuid",
        &[serde_json::json!(item_id)],
    )
    .is_empty()
}

// ─── Plumbing ────────────────────────────────────────────────────────

/// Read one field from an `application/x-www-form-urlencoded` body.
///
/// Written here because a plugin has no query-string parser: the kernel decodes the
/// query string for `ApiRequest::query`, and leaves a form body as the bytes that
/// arrived.
pub fn form_field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if percent_decode(key) == name {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

/// Decode `%XX` escapes and `+`, which is what a form body uses.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Escape for an HTML text node or a double-quoted attribute.
///
/// The kernel serves a plugin's response body as-is and does not sanitize it, and an
/// item title is whatever somebody typed, so escaping is this plugin's job.
pub fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wrap a fragment in a minimal document.
fn admin_page(title: &str, body: &str) -> ApiResponse {
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{t}</title>\n</head>\n<body>\n<h1>{t}</h1>\n{body}\n</body>\n</html>\n",
        t = escape_html(title),
    );
    ApiResponse::with_status(200, html).content_type("text/html; charset=utf-8")
}

/// A 303 back to a screen, so a form post does not leave the browser on a POST.
fn redirect(location: &str) -> ApiResponse {
    ApiResponse::with_status(
        303,
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\">\
             <meta http-equiv=\"refresh\" content=\"0;url={loc}\"></head>\
             <body><p><a href=\"{loc}\">Continue</a></p></body></html>",
            loc = escape_html(location)
        ),
    )
    .content_type("text/html; charset=utf-8")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn page(id: &str, parent: Option<&str>, weight: i64, title: &str) -> Page {
        Page {
            item_id: id.to_string(),
            book_id: "root".to_string(),
            parent_item_id: parent.map(String::from),
            weight,
            title: title.to_string(),
            depth: 0,
        }
    }

    /// A ten-page book, three levels, deliberately built out of order.
    fn fixture() -> Vec<Page> {
        vec![
            page("root", None, 0, "Handbook"),
            page("c", Some("root"), 20, "Part Three"),
            page("a", Some("root"), 0, "Part One"),
            page("b", Some("root"), 10, "Part Two"),
            page("a2", Some("a"), 10, "One Two"),
            page("a1", Some("a"), 0, "One One"),
            page("a1b", Some("a1"), 10, "One One B"),
            page("a1a", Some("a1"), 0, "One One A"),
            page("b1", Some("b"), 0, "Two One"),
            page("c1", Some("c"), 0, "Three One"),
        ]
    }

    #[test]
    fn the_reading_order_is_depth_first_by_weight_then_title() {
        let order = reading_order(&fixture());
        let ids: Vec<&str> = order.iter().map(|p| p.item_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["root", "a", "a1", "a1a", "a1b", "a2", "b", "b1", "c", "c1"]
        );
        let depths: Vec<usize> = order.iter().map(|p| p.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 3, 3, 2, 1, 2, 1, 2]);
    }

    /// The property prev/next depends on: every page once, and only once.
    #[test]
    fn the_reading_order_visits_every_page_exactly_once() {
        let pages = fixture();
        let order = reading_order(&pages);
        assert_eq!(order.len(), pages.len(), "every page must be visited");

        let mut ids: Vec<&str> = order.iter().map(|p| p.item_id.as_str()).collect();
        ids.sort_unstable();
        let mut expected: Vec<&str> = pages.iter().map(|p| p.item_id.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(ids, expected, "and no page twice");
    }

    #[test]
    fn siblings_at_the_same_weight_are_ordered_by_title() {
        let pages = vec![
            page("root", None, 0, "Book"),
            page("z", Some("root"), 0, "Alpha"),
            page("y", Some("root"), 0, "Beta"),
        ];
        let order = reading_order(&pages);
        let ids: Vec<&str> = order.iter().map(|p| p.item_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["root", "z", "y"],
            "the tie must break on title, so the order is total"
        );
    }

    #[test]
    fn the_trail_runs_from_the_root_down_to_the_parent() {
        let pages = fixture();
        let trail_pages = ancestors_of(&pages, "a1b");
        let trail: Vec<&str> = trail_pages.iter().map(|p| p.item_id.as_str()).collect();
        assert_eq!(trail, vec!["root", "a", "a1"]);
        assert!(
            ancestors_of(&pages, "root").is_empty(),
            "the root has no ancestors"
        );
    }

    #[test]
    fn a_page_cannot_be_placed_under_itself_or_its_descendants() {
        let pages = fixture();
        assert!(would_cycle(&pages, "a", "a"), "under itself");
        assert!(would_cycle(&pages, "a", "a1"), "under its child");
        assert!(would_cycle(&pages, "a", "a1b"), "under its grandchild");
        assert!(!would_cycle(&pages, "a", "b"), "under a sibling is fine");
        assert!(
            !would_cycle(&pages, "a1", "b1"),
            "under another branch is fine"
        );
        assert!(!would_cycle(&pages, "c1", "root"), "under the root is fine");
    }

    /// Orphan handling: a page whose parent is gone is still reachable, at the top.
    #[test]
    fn a_page_whose_parent_is_missing_is_treated_as_a_root() {
        let pages = vec![
            page("root", None, 0, "Book"),
            page("orphan", Some("gone"), 0, "Orphan"),
        ];
        let order = reading_order(&pages);
        assert_eq!(order.len(), 2, "the orphan must still be listed: {order:?}");
        assert!(order.iter().all(|p| p.depth == 0));
    }

    /// A pre-existing cycle does not hang and does not vanish.
    #[test]
    fn a_cyclic_pair_is_listed_rather_than_dropped_or_looped() {
        let pages = vec![page("x", Some("y"), 0, "X"), page("y", Some("x"), 0, "Y")];
        let order = reading_order(&pages);
        assert_eq!(order.len(), 2, "both pages must appear: {order:?}");
        assert!(
            ancestors_of(&pages, "x").len() <= 2,
            "the trail must terminate"
        );
    }

    #[test]
    fn the_tree_marks_the_current_page_and_nests_by_depth() {
        let html = render_tree(&fixture(), Some("a1a"));
        assert!(
            html.contains("book-tree__current"),
            "the current page is marked"
        );
        assert!(html.contains("aria-current=\"page\""), "and announced");
        // Three levels of nesting under the root means four open lists at the deepest.
        assert!(
            html.matches("<ul class=\"book-tree\">").count() >= 4,
            "the tree must nest, got: {html}"
        );
    }

    #[test]
    fn a_title_is_escaped_on_the_way_into_the_tree() {
        let pages = vec![page("root", None, 0, "<script>alert(1)</script>")];
        let html = render_tree(&pages, None);
        assert!(!html.contains("<script>"), "no raw markup may survive");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn escaping_neutralizes_markup_and_quotes() {
        assert_eq!(
            escape_html(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn a_form_body_is_decoded_field_by_field() {
        let body = "item_id=abc&parent_item_id=&weight=-3";
        assert_eq!(form_field(body, "item_id").as_deref(), Some("abc"));
        assert_eq!(form_field(body, "parent_item_id").as_deref(), Some(""));
        assert_eq!(form_field(body, "weight").as_deref(), Some("-3"));
        assert_eq!(form_field(body, "absent"), None);
    }

    #[test]
    fn a_form_body_decodes_escapes_and_plus() {
        let body = "title=Hello+World%21&id=a%2Db";
        assert_eq!(form_field(body, "title").as_deref(), Some("Hello World!"));
        assert_eq!(form_field(body, "id").as_deref(), Some("a-b"));
    }

    #[test]
    fn every_registered_route_is_an_api_route_with_a_callback() {
        for entry in __inner_tap_menu() {
            assert_eq!(entry.handler_type, "api", "{} must be routable", entry.path);
            assert!(
                !entry.callback.is_empty(),
                "{} needs a callback",
                entry.path
            );
            assert_eq!(
                entry.permission, PERM_ADMINISTER,
                "{} must be gated on the book permission",
                entry.path
            );
        }
    }

    #[test]
    fn an_unknown_callback_is_a_404() {
        let request = ApiRequest::new(
            "not_a_callback",
            "GET",
            "/admin/structure/books",
            "00000000-0000-0000-0000-000000000000",
            true,
        );
        assert_eq!(__inner_tap_api(request).status, 404);
    }

    /// An item that is not in a book gets no decoration, which is most items.
    #[test]
    fn an_item_outside_a_book_is_not_decorated() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "page".to_string(),
            title: "Loose page".to_string(),
            fields: std::collections::HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            created: 0,
            changed: 0,
            language: None,
            stage_id: Uuid::nil(),
        };
        // With the stub host, `query_raw` returns "[]", so there is no page row.
        assert!(__inner_tap_item_view(item).is_empty());
    }
}

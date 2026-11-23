/**
 * Registry field definitions → Puck fields.
 *
 * Every field type the kernel validates has one mapping here, so a plugin's
 * component gets a working form from its `.component.toml` alone:
 *
 *   text          → text, or textarea when `multiline`
 *   rich_text     → textarea with a preview rendered by the kernel
 *   number        → number (min/max)
 *   heading_level → number 2–6
 *   url           → text
 *   boolean       → Yes/No radio
 *   select        → select
 *   image         → media library picker, alt text, decorative
 *   link          → object { url, label, new_tab }
 *   list          → array of the sub fields
 *   reference     → item search or gather picker
 *
 * Hidden fields (kept only so stored pages validate) get no form.
 */

import { FieldLabel } from "@measured/puck";
import type { Field, Fields } from "@measured/puck";
import React, { useEffect, useState } from "react";

import type { FieldDefinition, ImageValue } from "./registry";

/** What a custom field needs from the page it is mounted on. */
export type EditorContext = {
  /** `POST` target that renders one component as the page would. */
  previewUrl: string;
};

const YES_NO = [
  { label: "Yes", value: true },
  { label: "No", value: false },
];

function labelOf(field: FieldDefinition): string | undefined {
  return field.label ?? undefined;
}

/** The Puck field for one registry field, or `null` for a hidden one. */
export function puckField(field: FieldDefinition, ctx: EditorContext, isKernel = true): Field | null {
  if (field.hidden) return null;
  const label = labelOf(field);
  const withLabel = <T extends object>(f: T): T => (label ? { ...f, label } : f);

  switch (field.type) {
    case "text":
      return withLabel({ type: field.multiline ? "textarea" : "text" } as Field);
    case "url":
      return withLabel({ type: "text" } as Field);
    case "rich_text":
      return withLabel({
        type: "custom",
        render: ({ name, value, onChange, readOnly }) => (
          <RichTextField
            label={label ?? name}
            value={(value as string) ?? ""}
            onChange={onChange}
            readOnly={readOnly}
            maxLength={field.max_length}
            ctx={ctx}
          />
        ),
      } as Field);
    case "number":
      return withLabel({
        type: "number",
        ...(field.min !== undefined ? { min: field.min } : {}),
        ...(field.max !== undefined ? { max: field.max } : {}),
      } as Field);
    case "heading_level":
      return withLabel({ type: "number", min: 2, max: 6 } as Field);
    case "boolean":
      return withLabel({ type: "radio", options: YES_NO } as Field);
    case "select":
      return withLabel({
        type: "select",
        options: field.options.map((o) => ({ label: o.label ?? o.value, value: o.value })),
      } as Field);
    case "image":
      return withLabel({
        type: "custom",
        render: ({ name, value, onChange, readOnly }) => (
          <ImageField
            label={label ?? name}
            help={field.help}
            value={(value as ImageValue) ?? {}}
            onChange={onChange}
            readOnly={readOnly}
          />
        ),
      } as Field);
    case "link":
      return withLabel({
        type: "object",
        objectFields: {
          url: { type: "text", label: "URL" },
          label: { type: "text", label: "Label" },
          new_tab: { type: "radio", label: "Open in a new tab", options: YES_NO },
        },
      } as Field);
    case "list": {
      const arrayFields = puckFields(field.fields, ctx, isKernel);
      const summaryField = field.fields.find(
        (f) => !f.hidden && (f.type === "text" || f.type === "rich_text"),
      );
      return withLabel({
        type: "array",
        arrayFields,
        ...(field.default_item ? { defaultItemProps: field.default_item } : {}),
        ...(field.min_items !== undefined ? { min: field.min_items } : {}),
        ...(field.max_items !== undefined ? { max: field.max_items } : {}),
        // The standard components keep Puck's own "Item n" summaries, as before.
        ...(!isKernel && summaryField
          ? {
              getItemSummary: (item: Record<string, unknown>, i?: number) =>
                (typeof item?.[summaryField.name] === "string" && (item[summaryField.name] as string)) ||
                `Item ${(i ?? 0) + 1}`,
            }
          : {}),
      } as Field);
    }
    case "reference":
      return withLabel({
        type: "custom",
        render: ({ name, value, onChange, readOnly }) => (
          <ReferenceField
            label={label ?? name}
            target={field.target}
            itemType={field.item_type}
            value={(value as string) ?? ""}
            onChange={onChange}
            readOnly={readOnly}
          />
        ),
      } as Field);
  }
}

/** The Puck fields for a list of registry fields, hidden ones left out. */
export function puckFields(fields: FieldDefinition[], ctx: EditorContext, isKernel = true): Fields {
  const out: Fields = {};
  for (const field of fields) {
    const f = puckField(field, ctx, isKernel);
    if (f) out[field.name] = f;
  }
  return out;
}

/** The props a new component starts with: each visible field's default. */
export function defaultProps(fields: FieldDefinition[]): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const field of fields) {
    if (!field.hidden && field.default !== undefined) out[field.name] = field.default;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Custom fields
// ---------------------------------------------------------------------------

const inputStyle: React.CSSProperties = {
  width: "100%",
  boxSizing: "border-box",
  padding: "6px 8px",
  border: "1px solid var(--puck-color-grey-09, #ccc)",
  borderRadius: 4,
  font: "inherit",
};

function RichTextField(props: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  readOnly?: boolean;
  maxLength?: number;
  ctx: EditorContext;
}) {
  const { label, value, onChange, readOnly, maxLength, ctx } = props;
  const [preview, setPreview] = useState<string | null>(null);
  const [showPreview, setShowPreview] = useState(false);

  useEffect(() => {
    if (!showPreview) return;
    const timer = setTimeout(() => {
      renderPreview(ctx, { type: "TextBlock", props: { content: value } })
        .then((r) => setPreview(r.html))
        .catch(() => setPreview(null));
    }, 250);
    return () => clearTimeout(timer);
  }, [value, showPreview, ctx]);

  const count = value.length;
  return (
    <FieldLabel label={label} el="div">
      <textarea
        style={{ ...inputStyle, minHeight: 120 }}
        value={value}
        readOnly={readOnly}
        onChange={(e) => onChange(e.currentTarget.value)}
      />
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, marginTop: 4 }}>
        <button type="button" onClick={() => setShowPreview((s) => !s)}>
          {showPreview ? "Hide preview" : "Preview"}
        </button>
        <span aria-live="polite" style={{ color: maxLength && count > maxLength ? "#b91c1c" : undefined }}>
          Markdown{maxLength ? ` · ${count}/${maxLength}` : ""}
        </span>
      </div>
      {showPreview && (
        <div
          className="pb-rich-text-preview"
          style={{ marginTop: 8, padding: 8, border: "1px dashed #ccc", borderRadius: 4 }}
          // The kernel's sanitized rendering of this Markdown.
          dangerouslySetInnerHTML={{ __html: preview ?? "" }}
        />
      )}
    </FieldLabel>
  );
}

type MediaPickerApi = {
  open: (opts: {
    type?: string;
    onSelect: (media: { url?: string; uri?: string; filename?: string; alt?: string }) => void;
  }) => void;
};

function ImageField(props: {
  label: string;
  help?: string;
  value: ImageValue;
  onChange: (v: ImageValue) => void;
  readOnly?: boolean;
}) {
  const { label, help, value, onChange, readOnly } = props;
  const picker = (window as unknown as { MediaPicker?: MediaPickerApi }).MediaPicker;
  const set = (patch: Partial<ImageValue>) => onChange({ ...value, ...patch });
  const missingAlt = !!value.src && !value.decorative && !(value.alt ?? "").trim();

  return (
    <FieldLabel label={label} el="div">
      {value.src ? (
        <img
          src={value.src}
          alt=""
          style={{ maxWidth: "100%", maxHeight: 120, display: "block", marginBottom: 6, borderRadius: 4 }}
        />
      ) : null}
      <div style={{ display: "flex", gap: 6, marginBottom: 6 }}>
        <button
          type="button"
          disabled={readOnly || !picker}
          onClick={() =>
            picker?.open({
              type: "image",
              onSelect: (media) => set({ src: media.url ?? media.uri ?? "" }),
            })
          }
        >
          Choose from media library
        </button>
        {value.src ? (
          <button type="button" disabled={readOnly} onClick={() => onChange({})}>
            Remove
          </button>
        ) : null}
      </div>
      <input
        style={inputStyle}
        aria-label={`${label}: image URL`}
        placeholder="/files/… or https://…"
        value={value.src ?? ""}
        readOnly={readOnly}
        onChange={(e) => set({ src: e.currentTarget.value })}
      />
      <input
        style={{ ...inputStyle, marginTop: 6 }}
        aria-label={`${label}: alt text`}
        placeholder="Alt text"
        value={value.alt ?? ""}
        readOnly={readOnly || value.decorative}
        aria-invalid={missingAlt}
        onChange={(e) => set({ alt: e.currentTarget.value })}
      />
      <label style={{ display: "flex", gap: 6, alignItems: "center", marginTop: 6, fontSize: 13 }}>
        <input
          type="checkbox"
          checked={!!value.decorative}
          disabled={readOnly}
          onChange={(e) => set({ decorative: e.currentTarget.checked })}
        />
        Decorative (no alt text)
      </label>
      {missingAlt ? (
        <p role="alert" style={{ color: "#b91c1c", fontSize: 12, margin: "4px 0 0" }}>
          An image needs alt text, or must be marked decorative.
        </p>
      ) : help ? (
        <p style={{ fontSize: 12, margin: "4px 0 0" }}>{help}</p>
      ) : null}
    </FieldLabel>
  );
}

type Choice = { id: string; title: string };

function ReferenceField(props: {
  label: string;
  target: "item" | "gather";
  itemType?: string;
  value: string;
  onChange: (v: string) => void;
  readOnly?: boolean;
}) {
  const { label, target, itemType, value, onChange, readOnly } = props;
  const [query, setQuery] = useState("");
  const [choices, setChoices] = useState<Choice[]>([]);

  useEffect(() => {
    if (target === "gather") {
      fetch("/api/v1/queries?per_page=500", { credentials: "same-origin" })
        .then((r) => (r.ok ? r.json() : { data: [] }))
        .then((page: { data: Array<{ query_id: string; label: string }> }) =>
          setChoices(page.data.map((q) => ({ id: q.query_id, title: q.label }))),
        )
        .catch(() => setChoices([]));
    }
  }, [target]);

  useEffect(() => {
    if (target !== "item" || !itemType || query.length < 2) return;
    const timer = setTimeout(() => {
      const params = new URLSearchParams({ type: itemType, q: query, per_page: "10" });
      fetch(`/api/v1/items/autocomplete?${params}`, { credentials: "same-origin" })
        .then((r) => (r.ok ? r.json() : { data: [] }))
        .then((page: { data: Choice[] }) => setChoices(page.data))
        .catch(() => setChoices([]));
    }, 200);
    return () => clearTimeout(timer);
  }, [query, target, itemType]);

  if (target === "gather") {
    return (
      <FieldLabel label={label} el="div">
        <select
          style={inputStyle}
          value={value}
          disabled={readOnly}
          onChange={(e) => onChange(e.currentTarget.value)}
        >
          <option value="">— None —</option>
          {choices.map((c) => (
            <option key={c.id} value={c.id}>
              {c.title}
            </option>
          ))}
          {value && !choices.some((c) => c.id === value) ? <option value={value}>{value}</option> : null}
        </select>
      </FieldLabel>
    );
  }

  return (
    <FieldLabel label={label} el="div">
      <input
        style={inputStyle}
        aria-label={`${label}: item id`}
        placeholder="Item id"
        value={value}
        readOnly={readOnly}
        onChange={(e) => onChange(e.currentTarget.value.trim())}
      />
      {itemType ? (
        <>
          <input
            style={{ ...inputStyle, marginTop: 6 }}
            aria-label={`${label}: search by title`}
            placeholder="Search by title…"
            value={query}
            readOnly={readOnly}
            onChange={(e) => setQuery(e.currentTarget.value)}
          />
          {choices.length > 0 ? (
            <ul style={{ listStyle: "none", padding: 0, margin: "4px 0 0" }}>
              {choices.map((c) => (
                <li key={c.id}>
                  <button
                    type="button"
                    onClick={() => {
                      onChange(c.id);
                      setChoices([]);
                      setQuery(c.title);
                    }}
                  >
                    {c.title}
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
        </>
      ) : null}
    </FieldLabel>
  );
}

// ---------------------------------------------------------------------------
// Server preview
// ---------------------------------------------------------------------------

/** The kernel's rendering of one component, and its validation errors. */
export type Preview = {
  html: string;
  stylesheets: string[];
  errors: Array<{ field: string; message: string }>;
};

/** Ask the kernel to render one component exactly as a page would. */
export async function renderPreview(
  ctx: EditorContext,
  component: { type: string; props: Record<string, unknown> },
): Promise<Preview> {
  const response = await fetch(ctx.previewUrl, {
    method: "POST",
    credentials: "same-origin",
    // No CSRF token: the endpoint changes nothing, and tokens are single
    // use, so sending the page's would spend the one the form posts.
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ component }),
  });
  if (!response.ok) throw new Error(`preview failed: ${response.status}`);
  return response.json();
}

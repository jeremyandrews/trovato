/**
 * The component registry as `/api/v1/page-builder/components` serves it.
 *
 * One entry per component the kernel accepts: the standard set and every
 * enabled plugin's, each declared in a `<name>.component.toml` file. The
 * editor builds its component list, categories and field forms from this,
 * so a plugin's component appears here without any editor change.
 */

export type SelectOption = { value: string; label?: string | null; class?: string };

export type FieldKind =
  | { type: "text"; max_length?: number; min_length?: number; multiline: boolean }
  | { type: "rich_text"; max_length?: number; min_heading: number }
  | { type: "number"; min?: number; max?: number; integer: boolean }
  | { type: "heading_level" }
  | { type: "url" }
  | { type: "image"; alt_max_length: number }
  | { type: "link"; label_max_length: number }
  | { type: "select"; options: SelectOption[] }
  | { type: "boolean" }
  | {
      type: "list";
      fields: FieldDefinition[];
      min_items?: number;
      max_items?: number;
      default_item?: Record<string, unknown>;
    }
  | { type: "reference"; target: "item" | "gather"; limit: number; item_type?: string };

export type FieldDefinition = FieldKind & {
  name: string;
  label?: string;
  help?: string;
  required: boolean;
  default?: unknown;
  hidden: boolean;
  deprecated?: string;
};

export type ComponentSource = { kind: "kernel" } | { kind: "plugin"; name: string };

export type ComponentDefinition = {
  type_name: string;
  label: string;
  category: string;
  description: string;
  fields: FieldDefinition[];
  zone_names: string[];
  has_zones: boolean;
  classes: string[];
  template: string;
  stylesheets: string[];
  in_editor: boolean;
  source: ComponentSource;
};

/** An image field's value. */
export type ImageValue = { src?: string; alt?: string; decorative?: boolean };

/** A link field's value. */
export type LinkValue = { url?: string; label?: string; new_tab?: boolean };

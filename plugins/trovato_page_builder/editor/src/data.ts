/**
 * Conversion between the page JSON the kernel stores and Puck's data.
 *
 * The kernel stores a component's drop zones inside it:
 *
 *   { type, props, zones: { "zone-0": [ ...children ] } }
 *
 * Puck keeps every zone in one page-level map keyed by `<parent id>:<zone>`.
 * Both directions are lossless for the props: a stored page opened and saved
 * without edits is the same page.
 */

export type StoredComponent = {
  type: string;
  props?: Record<string, unknown>;
  zones?: Record<string, StoredComponent[]>;
};

export type StoredPage = {
  root?: Record<string, unknown>;
  content?: StoredComponent[];
};

export type PuckComponentData = { type: string; props: Record<string, unknown> & { id: string } };

export type PuckData = {
  root: { props?: Record<string, unknown> };
  content: PuckComponentData[];
  zones?: Record<string, PuckComponentData[]>;
};

let counter = 0;

/** A component id Puck accepts, for a stored component that has none. */
export function newId(type: string): string {
  counter += 1;
  const random =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${counter}`;
  return `${type}-${random}`;
}

/** Parse the hidden input's value, falling back to an empty page. */
export function parseStored(raw: string | null | undefined): StoredPage {
  if (!raw || !raw.trim()) return { root: { title: "" }, content: [] };
  try {
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? parsed : { root: { title: "" }, content: [] };
  } catch {
    return { root: { title: "" }, content: [] };
  }
}

/** Stored page → Puck data. */
export function storedToPuck(page: StoredPage): PuckData {
  const zones: Record<string, PuckComponentData[]> = {};
  const convert = (component: StoredComponent): PuckComponentData => {
    const props = { ...(component.props ?? {}) } as Record<string, unknown>;
    const id = typeof props.id === "string" && props.id ? props.id : newId(component.type);
    props.id = id;
    for (const [zone, children] of Object.entries(component.zones ?? {})) {
      zones[`${id}:${zone}`] = (children ?? []).map(convert);
    }
    return { type: component.type, props: props as PuckComponentData["props"] };
  };
  const content = (page.content ?? []).map(convert);
  const { title, ...rest } = (page.root ?? {}) as Record<string, unknown>;
  return {
    root: { props: { title: typeof title === "string" ? title : "", ...rest } },
    content,
    zones,
  };
}

/** Puck data → the page JSON the kernel stores. */
export function puckToStored(data: PuckData): StoredPage {
  const zones = data.zones ?? {};
  const convert = (component: PuckComponentData): StoredComponent => {
    const prefix = `${component.props.id}:`;
    const own: Record<string, StoredComponent[]> = {};
    for (const [key, children] of Object.entries(zones)) {
      if (key.startsWith(prefix)) {
        own[key.slice(prefix.length)] = children.map(convert);
      }
    }
    const stored: StoredComponent = { type: component.type, props: { ...component.props } };
    if (Object.keys(own).length > 0) stored.zones = own;
    return stored;
  };
  return {
    root: { ...(data.root?.props ?? {}) },
    content: (data.content ?? []).map(convert),
  };
}

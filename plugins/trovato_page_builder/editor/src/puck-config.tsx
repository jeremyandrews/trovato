/**
 * Puck editor configuration for Trovato Page Builder, built from the kernel's
 * component registry.
 *
 * `buildConfig` reads the registry `/api/v1/page-builder/components` serves
 * and makes one Puck component per entry the editor offers: its label,
 * category, fields (see `fields.tsx`) and default props all come from the
 * component's `.component.toml`, so a plugin's components appear here with
 * no editor change.
 *
 * Rendering in the canvas: each standard component keeps its React render
 * below, which produces HTML structurally identical to its Tera template
 * (templates/pb/*.html). Any other component (every plugin's) is rendered by
 * the kernel itself through `/api/v1/page-builder/preview`, so the canvas
 * shows exactly what the page will: the plugin's template, the kernel's
 * normalization and sanitizer.
 *
 * Convention: component type names use PascalCase ("CardGrid", "Cta").
 * All-caps acronyms like "CTA" become "Cta" for clean kebab-case conversion.
 */

import type { Config } from "@measured/puck";
import React, { useEffect, useState } from "react";

import { defaultProps, puckFields, renderPreview, type EditorContext, type Preview } from "./fields";
import type { ComponentDefinition, FieldDefinition } from "./registry";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function zoneCount(layout: string): number {
  return (layout || "1/2+1/2").split("+").length;
}

function layoutClass(layout: string): string {
  return (layout || "1/2+1/2").replace(/\//g, "-").replace(/\+/g, "-");
}

/** The registry, by type name, for `variantClass`. Set by `buildConfig`. */
let registryIndex = new Map<string, ComponentDefinition>();

/**
 * The class a select field's chosen option adds, if the component declares
 * it: the same rule the kernel applies to `variant_classes`.
 */
export function variantClass(type: string, field: string, value: unknown): string {
  const def = registryIndex.get(type);
  const f = def?.fields.find((x) => x.name === field);
  if (!def || !f || f.type !== "select" || typeof value !== "string") return "";
  const cls = f.options.find((o) => o.value === value)?.class;
  return cls && def.classes.includes(cls) ? cls : "";
}

function withVariant(base: string, variant: string): string {
  return variant ? `${base} ${variant}` : base;
}

// ---------------------------------------------------------------------------
// The standard components' canvas renders
// ---------------------------------------------------------------------------

// Props are whatever the registry's fields hold, so they are not typed here.
/* eslint-disable @typescript-eslint/no-explicit-any */
type Render = (props: any) => React.ReactElement;

export const standardRenders: Record<string, Render> = {
  Hero: ({ title, subtitle, ctaText, ctaUrl, variant, background, headingLevel }) => {
    const H = `h${headingLevel || 2}` as keyof React.JSX.IntrinsicElements;
    return (
      <section className={withVariant(`pb-hero pb-hero--${variant || "standard"}`, variantClass("Hero", "background", background))}>
        <div className="pb-hero__content">
          {title && <H className="pb-hero__title">{title}</H>}
          {subtitle && <p className="pb-hero__subtitle">{subtitle}</p>}
          {ctaText && ctaUrl && <a className="pb-hero__cta" href={ctaUrl}>{ctaText}</a>}
        </div>
      </section>
    );
  },
  Columns: ({ layout, gapSize, puck: { renderDropZone } }) => {
    const count = zoneCount(layout);
    return (
      <div className={withVariant(`pb-columns pb-columns--${layoutClass(layout)}`, variantClass("Columns", "gapSize", gapSize))}>
        {Array.from({ length: count }, (_, i) => (
          <div key={i} className="pb-columns__zone">
            {renderDropZone({ zone: `zone-${i}` })}
          </div>
        ))}
      </div>
    );
  },
  TextBlock: ({ content }) => (
    <div className="pb-text-block">
      <p>{content}</p>
    </div>
  ),
  CardGrid: ({ columns, variant, cardHeadingLevel, cards }) => {
    const H = `h${cardHeadingLevel || 3}` as keyof React.JSX.IntrinsicElements;
    return (
      <div className={`pb-cards pb-cards--${variant} pb-cards--cols-${columns}`}>
        {(cards || []).map((card: any, i: number) => (
          <article key={i} className="pb-card">
            {card.image_url && variant !== "compact" && (
              <img src={card.image_url} alt={card.image_alt} className="pb-card__image" />
            )}
            <div className="pb-card__body">
              {card.title && <H className="pb-card__title">{card.title}</H>}
              {card.body && <p className="pb-card__text">{card.body}</p>}
              {card.link_url && (
                <a href={card.link_url} className="pb-card__link">{card.link_text || "Learn more"}</a>
              )}
            </div>
          </article>
        ))}
      </div>
    );
  },
  Cta: ({ heading, body, buttonText, buttonUrl, variant, background, headingLevel }) => {
    const H = `h${headingLevel || 2}` as keyof React.JSX.IntrinsicElements;
    return (
      <aside className={withVariant(`pb-cta pb-cta--${variant || "inline"}`, variantClass("Cta", "background", background))}>
        {heading && <H className="pb-cta__heading">{heading}</H>}
        {body && <p className="pb-cta__body">{body}</p>}
        {buttonText && buttonUrl && (
          <a href={buttonUrl} className="pb-cta__button">{buttonText}</a>
        )}
      </aside>
    );
  },
  Accordion: ({ items, allowMultiple }) => (
    <div className="pb-accordion">
      {(items || []).map((item: any, i: number) => (
        <details key={i} className="pb-accordion__item" {...(!allowMultiple ? { name: "accordion-group" } : {})}>
          <summary className="pb-accordion__title">{item.title}</summary>
          <div className="pb-accordion__content">{item.content}</div>
        </details>
      ))}
    </div>
  ),
  ContentFeature: ({ title, body, imageUrl, imageAlt, imagePosition, linkUrl, linkText, headingLevel }) => {
    const H = `h${headingLevel || 2}` as keyof React.JSX.IntrinsicElements;
    return (
      <div className={`pb-feature pb-feature--img-${imagePosition || "left"}`}>
        {imageUrl && (
          <div className="pb-feature__image">
            <img src={imageUrl} alt={imageAlt || ""} />
          </div>
        )}
        <div className="pb-feature__text">
          {title && <H className="pb-feature__title">{title}</H>}
          {body && <div className="pb-feature__body">{body}</div>}
          {linkUrl && <a href={linkUrl} className="pb-feature__link">{linkText || "Learn more"}</a>}
        </div>
      </div>
    );
  },
  LogoRow: ({ title, logos, headingLevel }) => {
    const H = `h${headingLevel || 3}` as keyof React.JSX.IntrinsicElements;
    return (
      <div className="pb-logos">
        {title && <H className="pb-logos__title">{title}</H>}
        <div className="pb-logos__row" role="list">
          {(logos || []).map((logo: any, i: number) => (
            <div key={i} role="listitem">
              {logo.link_url ? (
                <a href={logo.link_url} className="pb-logos__link">
                  <img src={logo.image_url} alt={logo.alt} className="pb-logos__img" />
                </a>
              ) : (
                <img src={logo.image_url} alt={logo.alt} className="pb-logos__img" />
              )}
            </div>
          ))}
        </div>
      </div>
    );
  },
  SummaryBox: ({ title, content, variant, headingLevel }) => {
    const H = `h${headingLevel || 3}` as keyof React.JSX.IntrinsicElements;
    return (
      <aside className={`pb-summary pb-summary--${variant || "info"}`}>
        {title && <H className="pb-summary__title">{title}</H>}
        {content && <div className="pb-summary__content">{content}</div>}
      </aside>
    );
  },
  SectionWrapper: ({ padding, maxWidth, ariaLabel, background, puck: { renderDropZone } }) => (
    <section
      className={withVariant(`pb-section pb-section--pad-${padding || "default"} pb-section--width-${maxWidth || "default"}`, variantClass("SectionWrapper", "background", background))}
      aria-label={ariaLabel || undefined}
    >
      {renderDropZone({ zone: "content" })}
    </section>
  ),
  BlockquoteExtended: ({ text, attribution, role, imageUrl }) => (
    <figure className="pb-blockquote">
      <blockquote className="pb-blockquote__text">{text}</blockquote>
      {attribution && (
        <figcaption className="pb-blockquote__attribution">
          {imageUrl && <img src={imageUrl} alt={attribution} className="pb-blockquote__avatar" />}
          <span className="pb-blockquote__name">{attribution}</span>
          {role && <span className="pb-blockquote__role">{role}</span>}
        </figcaption>
      )}
    </figure>
  ),
  YouTubeEmbed: ({ videoId, title, aspectRatio, transcriptUrl }) => {
    const arClass = (aspectRatio || "16:9").replace(":", "-");
    return (
      <figure className={`pb-youtube pb-youtube--${arClass}`}>
        {videoId && (
          <iframe
            src={`https://www.youtube-nocookie.com/embed/${videoId}`}
            title={title || "Video"}
            frameBorder="0"
            allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture"
            allowFullScreen
            loading="lazy"
          />
        )}
        {transcriptUrl && (
          <figcaption className="pb-youtube__transcript">
            <a href={transcriptUrl}>View transcript</a>
          </figcaption>
        )}
      </figure>
    );
  },
};

// ---------------------------------------------------------------------------
// Any other component: the kernel's own rendering
// ---------------------------------------------------------------------------

/** Link a stylesheet a component declared, once per page. */
function ensureStylesheet(href: string): void {
  if (document.querySelector(`link[rel="stylesheet"][href="${CSS.escape(href)}"]`)) return;
  const link = document.createElement("link");
  link.rel = "stylesheet";
  link.href = href;
  document.head.appendChild(link);
}

function ServerPreview(props: {
  def: ComponentDefinition;
  componentProps: Record<string, unknown>;
  ctx: EditorContext;
  renderDropZone?: (opts: { zone: string }) => React.ReactNode;
}) {
  const { def, componentProps, ctx, renderDropZone } = props;
  const [preview, setPreview] = useState<Preview | null>(null);
  const [failed, setFailed] = useState(false);
  const key = JSON.stringify(componentProps);

  useEffect(() => {
    const timer = setTimeout(() => {
      renderPreview(ctx, { type: def.type_name, props: componentProps })
        .then((p) => {
          p.stylesheets.forEach(ensureStylesheet);
          setPreview(p);
          setFailed(false);
        })
        .catch(() => setFailed(true));
    }, 200);
    return () => clearTimeout(timer);
    // `key` stands for the props' content.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, def.type_name, ctx]);

  return (
    <div className="pb-server-preview" data-component={def.type_name}>
      {failed ? <p className="pb-server-preview__note">Preview unavailable.</p> : null}
      {preview && preview.errors.length > 0 ? (
        <ul className="pb-server-preview__errors" role="alert">
          {preview.errors.map((e, i) => (
            <li key={i}>
              {e.field ? `${e.field}: ` : ""}
              {e.message}
            </li>
          ))}
        </ul>
      ) : null}
      {/* The kernel's sanitized rendering of this component. */}
      <div dangerouslySetInnerHTML={{ __html: preview?.html ?? "" }} />
      {renderDropZone
        ? def.zone_names.map((zone) => (
            <div key={zone} className="pb-server-preview__zone">
              {renderDropZone({ zone })}
            </div>
          ))
        : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// The config
// ---------------------------------------------------------------------------

/**
 * The page settings form (Puck's root fields): the built in title, then every
 * page root field the enabled plugins declare, with the same field mapping
 * components use, each starting at its declared default.
 */
export function rootConfig(rootFields: FieldDefinition[], ctx: EditorContext): NonNullable<Config<any>["root"]> {
  return {
    fields: {
      title: { type: "text", label: "Title" },
      ...puckFields(rootFields, ctx, false),
    },
    defaultProps: { title: "", ...defaultProps(rootFields) },
  };
}

/**
 * Build the Puck config from the registry. Components with `in_editor =
 * false` stay out of the picker (a stored page holding one still loads).
 * Categories appear in registry order: the standard set's first, then each
 * plugin's. `rootFields` are the page settings form's fields after the title.
 */
export function buildConfig(
  registry: ComponentDefinition[],
  ctx: EditorContext,
  rootFields: FieldDefinition[] = [],
): Config<any> {
  registryIndex = new Map(registry.map((d) => [d.type_name, d]));
  const components: Config<any>["components"] = {};
  const categories: Record<string, { components: string[]; visible?: boolean; title?: string }> = {};
  const hidden: string[] = [];

  for (const def of registry) {
    const isKernel = def.source.kind === "kernel";
    const standard = isKernel ? standardRenders[def.type_name] : undefined;
    components[def.type_name] = {
      label: def.label,
      fields: puckFields(def.fields, ctx, isKernel),
      defaultProps: defaultProps(def.fields),
      render:
        standard ??
        ((renderProps: any) => {
          const { puck, id: _id, editMode: _edit, ...componentProps } = renderProps;
          return (
            <ServerPreview
              def={def}
              componentProps={componentProps}
              ctx={ctx}
              renderDropZone={def.has_zones ? puck?.renderDropZone : undefined}
            />
          );
        }),
    };
    if (!def.in_editor) {
      hidden.push(def.type_name);
      continue;
    }
    (categories[def.category] ??= { components: [] }).components.push(def.type_name);
  }
  // Loadable from a stored page, never offered in the picker.
  if (hidden.length > 0) categories.not_offered = { components: hidden, visible: false };

  return { categories, components, root: rootConfig(rootFields, ctx) };
}

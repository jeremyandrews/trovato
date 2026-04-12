/**
 * Puck editor configuration for Trovato Page Builder.
 *
 * Defines all available components, their prop fields, default values,
 * and React render functions. Each component's render() must produce HTML
 * structurally identical to its corresponding Tera template (templates/pb/*.html).
 *
 * Convention: component type names use PascalCase ("CardGrid", "Cta").
 * All-caps acronyms like "CTA" become "Cta" for clean kebab-case conversion.
 */

import type { Config } from "@measured/puck";
import { DropZone } from "@measured/puck";
import React from "react";

// ---------------------------------------------------------------------------
// Type definitions
// ---------------------------------------------------------------------------

type Components = {
  Hero: {
    title: string;
    subtitle: string;
    backgroundImage: string;
    imageAlt: string;
    ctaText: string;
    ctaUrl: string;
    variant: "standard" | "split" | "minimal";
    backgroundColor: string;
    headingLevel: number;
    lang: string;
  };
  Columns: {
    layout: string;
    gap: string;
  };
  TextBlock: {
    content: string;
    lang: string;
  };
  CardGrid: {
    columns: number;
    variant: "standard" | "feature" | "compact";
    cardHeadingLevel: number;
    cards: Array<{
      title: string;
      body: string;
      image_url: string;
      image_alt: string;
      link_url: string;
      link_text: string;
    }>;
    lang: string;
  };
  Cta: {
    heading: string;
    body: string;
    buttonText: string;
    buttonUrl: string;
    variant: "inline" | "fullWidth" | "callout";
    backgroundColor: string;
    headingLevel: number;
    lang: string;
  };
  Accordion: {
    allowMultiple: boolean;
    items: Array<{ title: string; content: string }>;
    lang: string;
  };
  ContentFeature: {
    title: string;
    body: string;
    imageUrl: string;
    imageAlt: string;
    imagePosition: "left" | "right";
    linkUrl: string;
    linkText: string;
    headingLevel: number;
    lang: string;
  };
  LogoRow: {
    title: string;
    headingLevel: number;
    logos: Array<{ image_url: string; alt: string; link_url: string }>;
    lang: string;
  };
  SummaryBox: {
    title: string;
    content: string;
    variant: "info" | "warning" | "success";
    headingLevel: number;
    lang: string;
  };
  SectionWrapper: {
    backgroundColor: string;
    padding: "default" | "large" | "none";
    maxWidth: "default" | "wide" | "full";
    ariaLabel: string;
    lang: string;
  };
  BlockquoteExtended: {
    text: string;
    attribution: string;
    role: string;
    imageUrl: string;
  };
  YouTubeEmbed: {
    videoId: string;
    title: string;
    aspectRatio: "16:9" | "4:3";
    transcriptUrl: string;
    lang: string;
  };
};

// ---------------------------------------------------------------------------
// Helper: zone count from column layout
// ---------------------------------------------------------------------------

function zoneCount(layout: string): number {
  return (layout || "1/2+1/2").split("+").length;
}

function layoutClass(layout: string): string {
  return (layout || "1/2+1/2").replace(/\//g, "-").replace(/\+/g, "-");
}

// ---------------------------------------------------------------------------
// Puck config
// ---------------------------------------------------------------------------

export const config: Config<Components> = {
  categories: {
    layout: { components: ["Columns", "SectionWrapper"] },
    content: { components: ["Hero", "TextBlock", "Cta", "ContentFeature", "BlockquoteExtended"] },
    media: { components: ["YouTubeEmbed", "LogoRow"] },
    data: { components: ["CardGrid", "Accordion", "SummaryBox"] },
  },
  components: {
    // ------ Priority 1 ------
    Hero: {
      fields: {
        title: { type: "text" },
        subtitle: { type: "text" },
        backgroundImage: { type: "text", label: "Background Image URL" },
        imageAlt: { type: "text", label: "Image Alt Text" },
        ctaText: { type: "text", label: "CTA Button Text" },
        ctaUrl: { type: "text", label: "CTA Button URL" },
        variant: {
          type: "select",
          options: [
            { label: "Standard", value: "standard" },
            { label: "Split (50/50)", value: "split" },
            { label: "Minimal", value: "minimal" },
          ],
        },
        backgroundColor: { type: "text", label: "Background Color" },
        headingLevel: { type: "number", label: "Heading Level (2-6)", min: 2, max: 6 },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { title: "Hero Title", variant: "standard", headingLevel: 2 },
      render: ({ title, subtitle, ctaText, ctaUrl, variant, headingLevel }) => {
        const H = `h${headingLevel || 2}` as keyof JSX.IntrinsicElements;
        return (
          <section className={`pb-hero pb-hero--${variant || "standard"}`}>
            <div className="pb-hero__content">
              {title && <H className="pb-hero__title">{title}</H>}
              {subtitle && <p className="pb-hero__subtitle">{subtitle}</p>}
              {ctaText && ctaUrl && <a className="pb-hero__cta" href={ctaUrl}>{ctaText}</a>}
            </div>
          </section>
        );
      },
    },

    Columns: {
      fields: {
        layout: {
          type: "select",
          options: [
            { label: "50/50", value: "1/2+1/2" },
            { label: "2/3 + 1/3", value: "2/3+1/3" },
            { label: "1/3 + 2/3", value: "1/3+2/3" },
            { label: "Thirds", value: "1/3+1/3+1/3" },
            { label: "Quarters", value: "1/4+1/4+1/4+1/4" },
          ],
        },
        gap: { type: "text", label: "Gap (CSS value)" },
      },
      defaultProps: { layout: "1/2+1/2", gap: "2rem" },
      render: ({ layout, gap, puck: { renderDropZone } }) => {
        const count = zoneCount(layout);
        return (
          <div className={`pb-columns pb-columns--${layoutClass(layout)}`} style={{ gap }}>
            {Array.from({ length: count }, (_, i) => (
              <div key={i} className="pb-columns__zone">
                {renderDropZone({ zone: `zone-${i}` })}
              </div>
            ))}
          </div>
        );
      },
    },

    TextBlock: {
      fields: {
        content: { type: "textarea" },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { content: "Write **Markdown** here." },
      render: ({ content }) => (
        <div className="pb-text-block">
          <p>{content}</p>
        </div>
      ),
    },

    CardGrid: {
      fields: {
        columns: { type: "number", label: "Columns (2-4)", min: 2, max: 4 },
        variant: {
          type: "select",
          options: [
            { label: "Standard", value: "standard" },
            { label: "Feature", value: "feature" },
            { label: "Compact", value: "compact" },
          ],
        },
        cardHeadingLevel: { type: "number", label: "Card Heading Level (2-6)", min: 2, max: 6 },
        cards: {
          type: "array",
          arrayFields: {
            title: { type: "text" },
            body: { type: "textarea" },
            image_url: { type: "text", label: "Image URL" },
            image_alt: { type: "text", label: "Image Alt" },
            link_url: { type: "text", label: "Link URL" },
            link_text: { type: "text", label: "Link Text" },
          },
          defaultItemProps: { title: "Card Title", body: "Card description." },
        },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { columns: 3, variant: "standard", cardHeadingLevel: 3, cards: [] },
      render: ({ columns, variant, cardHeadingLevel, cards }) => {
        const H = `h${cardHeadingLevel || 3}` as keyof JSX.IntrinsicElements;
        return (
          <div className={`pb-cards pb-cards--${variant} pb-cards--cols-${columns}`}>
            {(cards || []).map((card, i) => (
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
    },

    Cta: {
      fields: {
        heading: { type: "text" },
        body: { type: "textarea" },
        buttonText: { type: "text", label: "Button Text" },
        buttonUrl: { type: "text", label: "Button URL" },
        variant: {
          type: "select",
          options: [
            { label: "Inline", value: "inline" },
            { label: "Full Width", value: "fullWidth" },
            { label: "Callout", value: "callout" },
          ],
        },
        backgroundColor: { type: "text", label: "Background Color" },
        headingLevel: { type: "number", label: "Heading Level (2-6)", min: 2, max: 6 },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { heading: "Call to Action", variant: "inline", headingLevel: 2 },
      render: ({ heading, body, buttonText, buttonUrl, variant, headingLevel }) => {
        const H = `h${headingLevel || 2}` as keyof JSX.IntrinsicElements;
        return (
          <aside className={`pb-cta pb-cta--${variant || "inline"}`}>
            {heading && <H className="pb-cta__heading">{heading}</H>}
            {body && <p className="pb-cta__body">{body}</p>}
            {buttonText && buttonUrl && (
              <a href={buttonUrl} className="pb-cta__button">{buttonText}</a>
            )}
          </aside>
        );
      },
    },

    Accordion: {
      fields: {
        allowMultiple: { type: "radio", options: [
          { label: "Yes", value: true },
          { label: "No", value: false },
        ]},
        items: {
          type: "array",
          arrayFields: {
            title: { type: "text" },
            content: { type: "textarea" },
          },
          defaultItemProps: { title: "Section Title", content: "Section content." },
        },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { allowMultiple: false, items: [] },
      render: ({ items, allowMultiple }) => (
        <div className="pb-accordion">
          {(items || []).map((item, i) => (
            <details key={i} className="pb-accordion__item" {...(!allowMultiple ? { name: "accordion-group" } : {})}>
              <summary className="pb-accordion__title">{item.title}</summary>
              <div className="pb-accordion__content">{item.content}</div>
            </details>
          ))}
        </div>
      ),
    },

    ContentFeature: {
      fields: {
        title: { type: "text" },
        body: { type: "textarea" },
        imageUrl: { type: "text", label: "Image URL" },
        imageAlt: { type: "text", label: "Image Alt Text (required)" },
        imagePosition: {
          type: "select",
          options: [
            { label: "Left", value: "left" },
            { label: "Right", value: "right" },
          ],
        },
        linkUrl: { type: "text", label: "Link URL" },
        linkText: { type: "text", label: "Link Text" },
        headingLevel: { type: "number", label: "Heading Level (2-6)", min: 2, max: 6 },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { imagePosition: "left", headingLevel: 2 },
      render: ({ title, body, imageUrl, imageAlt, imagePosition, linkUrl, linkText, headingLevel }) => {
        const H = `h${headingLevel || 2}` as keyof JSX.IntrinsicElements;
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
    },

    // ------ Priority 2 ------
    LogoRow: {
      fields: {
        title: { type: "text" },
        headingLevel: { type: "number", label: "Heading Level (2-6)", min: 2, max: 6 },
        logos: {
          type: "array",
          arrayFields: {
            image_url: { type: "text", label: "Image URL" },
            alt: { type: "text", label: "Alt Text (required)" },
            link_url: { type: "text", label: "Link URL" },
          },
          defaultItemProps: { alt: "Logo" },
        },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { headingLevel: 3, logos: [] },
      render: ({ title, logos, headingLevel }) => {
        const H = `h${headingLevel || 3}` as keyof JSX.IntrinsicElements;
        return (
          <div className="pb-logos">
            {title && <H className="pb-logos__title">{title}</H>}
            <div className="pb-logos__row" role="list">
              {(logos || []).map((logo, i) => (
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
    },

    SummaryBox: {
      fields: {
        title: { type: "text" },
        content: { type: "textarea" },
        variant: {
          type: "select",
          options: [
            { label: "Info", value: "info" },
            { label: "Warning", value: "warning" },
            { label: "Success", value: "success" },
          ],
        },
        headingLevel: { type: "number", label: "Heading Level (2-6)", min: 2, max: 6 },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { variant: "info", headingLevel: 3 },
      render: ({ title, content, variant, headingLevel }) => {
        const H = `h${headingLevel || 3}` as keyof JSX.IntrinsicElements;
        return (
          <aside className={`pb-summary pb-summary--${variant || "info"}`}>
            {title && <H className="pb-summary__title">{title}</H>}
            {content && <div className="pb-summary__content">{content}</div>}
          </aside>
        );
      },
    },

    SectionWrapper: {
      fields: {
        backgroundColor: { type: "text", label: "Background Color" },
        padding: {
          type: "select",
          options: [
            { label: "Default", value: "default" },
            { label: "Large", value: "large" },
            { label: "None", value: "none" },
          ],
        },
        maxWidth: {
          type: "select",
          options: [
            { label: "Default", value: "default" },
            { label: "Wide", value: "wide" },
            { label: "Full", value: "full" },
          ],
        },
        ariaLabel: { type: "text", label: "Section Label (accessibility)" },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { padding: "default", maxWidth: "default" },
      render: ({ padding, maxWidth, ariaLabel, backgroundColor, puck: { renderDropZone } }) => (
        <section
          className={`pb-section pb-section--pad-${padding || "default"} pb-section--width-${maxWidth || "default"}`}
          aria-label={ariaLabel || undefined}
          style={backgroundColor ? { backgroundColor } : undefined}
        >
          {renderDropZone({ zone: "content" })}
        </section>
      ),
    },

    BlockquoteExtended: {
      fields: {
        text: { type: "textarea" },
        attribution: { type: "text" },
        role: { type: "text", label: "Role / Title" },
        imageUrl: { type: "text", label: "Avatar Image URL" },
      },
      defaultProps: { text: "Quote text here." },
      render: ({ text, attribution, role, imageUrl }) => (
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
    },

    YouTubeEmbed: {
      fields: {
        videoId: { type: "text", label: "YouTube Video ID" },
        title: { type: "text", label: "Video Title (required for accessibility)" },
        aspectRatio: {
          type: "select",
          options: [
            { label: "16:9", value: "16:9" },
            { label: "4:3", value: "4:3" },
          ],
        },
        transcriptUrl: { type: "text", label: "Transcript URL" },
        lang: { type: "text", label: "Language Override" },
      },
      defaultProps: { aspectRatio: "16:9" },
      render: ({ videoId, title, aspectRatio, transcriptUrl }) => {
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
    },
  },
};

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { buildConfig, variantClass } from "./puck-config";
import type { ComponentDefinition } from "./registry";

// The standard registry exactly as the kernel serves it (the kernel test
// `the_editor_fixture_is_the_standard_registry` keeps this file current).
const standard: ComponentDefinition[] = JSON.parse(
  readFileSync(resolve(__dirname, "__fixtures__/standard-registry.json"), "utf8"),
).data;

const ctx = { previewUrl: "/api/v1/page-builder/preview" };

describe("the standard components, built from the registry", () => {
  const config = buildConfig(standard, ctx);

  it("keep the categories the hard coded config had", () => {
    expect(config.categories).toEqual({
      layout: { components: ["Columns", "SectionWrapper"] },
      content: { components: ["Hero", "TextBlock", "Cta", "ContentFeature", "BlockquoteExtended"] },
      media: { components: ["YouTubeEmbed", "LogoRow"] },
      data: { components: ["CardGrid", "Accordion", "SummaryBox"] },
      not_offered: { components: ["TableOfContents"], visible: false },
    });
  });

  it("keep their fields, with the style props replaced by variants", () => {
    const keys = (type: string) => Object.keys(config.components[type].fields ?? {});
    expect(keys("Hero")).toEqual([
      "title", "subtitle", "backgroundImage", "imageAlt", "ctaText", "ctaUrl", "variant",
      "background", "headingLevel", "lang",
    ]);
    expect(keys("Columns")).toEqual(["layout", "gapSize"]);
    expect(keys("SectionWrapper")).toEqual(["background", "padding", "maxWidth", "ariaLabel", "lang"]);
    expect(keys("Cta")).toEqual([
      "heading", "body", "buttonText", "buttonUrl", "variant", "background", "headingLevel", "lang",
    ]);
    expect(keys("CardGrid")).toEqual(["columns", "variant", "cardHeadingLevel", "cards", "lang"]);
    expect(keys("Accordion")).toEqual(["allowMultiple", "items", "lang"]);
    expect(keys("ContentFeature")).toEqual([
      "title", "body", "imageUrl", "imageAlt", "imagePosition", "linkUrl", "linkText", "headingLevel", "lang",
    ]);
    expect(keys("LogoRow")).toEqual(["title", "headingLevel", "logos", "lang"]);
    expect(keys("SummaryBox")).toEqual(["title", "content", "variant", "headingLevel", "lang"]);
    expect(keys("BlockquoteExtended")).toEqual(["text", "attribution", "role", "imageUrl"]);
    expect(keys("YouTubeEmbed")).toEqual(["videoId", "title", "aspectRatio", "transcriptUrl", "lang"]);
    expect(keys("TextBlock")).toEqual(["content", "lang"]);
  });

  it("keep their field types, labels and limits", () => {
    const hero = config.components.Hero.fields as Record<string, any>;
    expect(hero.title).toEqual({ type: "text" });
    expect(hero.backgroundImage).toEqual({ type: "text", label: "Background Image URL" });
    expect(hero.headingLevel).toEqual({ type: "number", min: 2, max: 6, label: "Heading Level (2-6)" });
    expect(hero.variant.options).toEqual([
      { label: "Standard", value: "standard" },
      { label: "Split (50/50)", value: "split" },
      { label: "Minimal", value: "minimal" },
    ]);
    const cards = (config.components.CardGrid.fields as Record<string, any>).cards;
    expect(cards.type).toBe("array");
    expect(Object.keys(cards.arrayFields)).toEqual(["title", "body", "image_url", "image_alt", "link_url", "link_text"]);
    expect(cards.arrayFields.body).toEqual({ type: "textarea" });
    expect(cards.defaultItemProps).toEqual({ title: "Card Title", body: "Card description." });
    expect(cards.getItemSummary).toBeUndefined();
    const accordion = config.components.Accordion.fields as Record<string, any>;
    expect(accordion.allowMultiple).toEqual({
      type: "radio",
      options: [
        { label: "Yes", value: true },
        { label: "No", value: false },
      ],
    });
    expect((config.components.CardGrid.fields as Record<string, any>).columns).toEqual({
      type: "number", min: 2, max: 4, label: "Columns (2-4)",
    });
  });

  it("keep their default props", () => {
    const d = (type: string) => config.components[type].defaultProps;
    expect(d("Hero")).toEqual({ title: "Hero Title", variant: "standard", background: "", headingLevel: 2 });
    expect(d("Columns")).toEqual({ layout: "1/2+1/2", gapSize: "default" });
    expect(d("TextBlock")).toEqual({ content: "Write **Markdown** here." });
    expect(d("CardGrid")).toEqual({ columns: 3, variant: "standard", cardHeadingLevel: 3, cards: [] });
    expect(d("Cta")).toEqual({ heading: "Call to Action", variant: "inline", background: "", headingLevel: 2 });
    expect(d("Accordion")).toEqual({ allowMultiple: false, items: [] });
    expect(d("ContentFeature")).toEqual({ imagePosition: "left", headingLevel: 2 });
    expect(d("LogoRow")).toEqual({ headingLevel: 3, logos: [] });
    expect(d("SummaryBox")).toEqual({ variant: "info", headingLevel: 3 });
    expect(d("SectionWrapper")).toEqual({ background: "", padding: "default", maxWidth: "default" });
    expect(d("BlockquoteExtended")).toEqual({ text: "Quote text here." });
    expect(d("YouTubeEmbed")).toEqual({ aspectRatio: "16:9" });
  });

  it("add a variant's class only when the component declares it", () => {
    expect(variantClass("Cta", "background", "dark")).toBe("pb-bg--dark");
    expect(variantClass("Cta", "background", "")).toBe("");
    expect(variantClass("Cta", "background", "neon")).toBe("");
    expect(variantClass("Columns", "gapSize", "large")).toBe("pb-columns--gap-large");
  });
});

describe("a plugin component, built from its registry entry", () => {
  const plugin: ComponentDefinition = {
    type_name: "PromoCards",
    label: "Promo cards",
    category: "marketing",
    description: "",
    zone_names: [],
    has_zones: false,
    classes: ["promo--dark"],
    template: "pb/promo-cards.html",
    stylesheets: ["/static/plugins/site/promo.css"],
    in_editor: true,
    source: { kind: "plugin", name: "site" },
    fields: [
      { name: "heading", type: "text", multiline: false, max_length: 80, required: false, hidden: false },
      { name: "intro", type: "rich_text", min_heading: 2, required: false, hidden: false },
      { name: "level", type: "heading_level", required: false, hidden: false, default: 2 },
      { name: "featured", type: "boolean", required: false, hidden: false },
      {
        name: "tone", type: "select", required: false, hidden: false, default: "light",
        options: [{ value: "light", label: "Light" }, { value: "dark", class: "promo--dark" }],
      },
      { name: "photo", type: "image", alt_max_length: 250, required: false, hidden: false },
      { name: "cta", type: "link", label_max_length: 120, required: false, hidden: false },
      { name: "story", type: "reference", target: "item", limit: 10, item_type: "page", required: false, hidden: false },
      { name: "latest", type: "reference", target: "gather", limit: 5, required: false, hidden: false },
      { name: "old", type: "text", multiline: false, required: false, hidden: true },
      {
        name: "cards", type: "list", required: false, hidden: false, max_items: 6,
        default_item: { title: "New card" },
        fields: [
          { name: "eyebrow", type: "text", multiline: false, required: false, hidden: false },
          { name: "title", type: "text", multiline: false, required: true, hidden: false },
          { name: "link", type: "link", label_max_length: 120, required: false, hidden: false },
        ],
      },
    ],
  };
  const config = buildConfig([...standard, plugin], ctx);
  const fields = config.components.PromoCards.fields as Record<string, any>;

  it("is offered in its own category, after the standard ones", () => {
    expect(Object.keys(config.categories ?? {})).toEqual(["layout", "content", "media", "data", "marketing", "not_offered"]);
    expect(config.categories?.marketing).toEqual({ components: ["PromoCards"] });
    expect(config.components.PromoCards.label).toBe("Promo cards");
  });

  it("maps every field type to a Puck field and leaves hidden ones out", () => {
    expect(Object.keys(fields)).toEqual([
      "heading", "intro", "level", "featured", "tone", "photo", "cta", "story", "latest", "cards",
    ]);
    expect(fields.heading).toEqual({ type: "text" });
    expect(fields.intro.type).toBe("custom");
    expect(fields.level).toEqual({ type: "number", min: 2, max: 6 });
    expect(fields.featured.type).toBe("radio");
    expect(fields.tone).toEqual({
      type: "select",
      options: [{ label: "Light", value: "light" }, { label: "dark", value: "dark" }],
    });
    expect(fields.photo.type).toBe("custom");
    expect(fields.cta.type).toBe("object");
    expect(Object.keys(fields.cta.objectFields)).toEqual(["url", "label", "new_tab"]);
    expect(fields.story.type).toBe("custom");
    expect(fields.latest.type).toBe("custom");
  });

  it("maps a repeatable list to an array of its sub fields", () => {
    expect(fields.cards.type).toBe("array");
    expect(fields.cards.max).toBe(6);
    expect(Object.keys(fields.cards.arrayFields)).toEqual(["eyebrow", "title", "link"]);
    expect(fields.cards.arrayFields.link.type).toBe("object");
    expect(fields.cards.defaultItemProps).toEqual({ title: "New card" });
    expect(fields.cards.getItemSummary({ eyebrow: "New", title: "T" }, 0)).toBe("New");
    expect(fields.cards.getItemSummary({}, 2)).toBe("Item 3");
  });

  it("starts with the declared defaults", () => {
    expect(config.components.PromoCards.defaultProps).toEqual({ level: 2, tone: "light" });
  });
});

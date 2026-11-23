import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { parseStored, puckToStored, storedToPuck } from "./data";

const fixture = (name: string) =>
  JSON.parse(
    readFileSync(resolve(__dirname, "../../../../crates/kernel/tests/fixtures/page_builder", name), "utf8"),
  );

describe("stored page ↔ Puck data", () => {
  it("round-trips a page saved by v0.106 without changing it", () => {
    const page = fixture("standard-page-v0.106.json");
    expect(puckToStored(storedToPuck(page))).toEqual(page);
  });

  it("moves inline zones into Puck's page-level map, keyed by parent id", () => {
    const data = storedToPuck({
      content: [
        {
          type: "Columns",
          props: { id: "Columns-1", layout: "1/2+1/2" },
          zones: { "zone-0": [{ type: "Cta", props: { id: "Cta-1", heading: "Hi" } }], "zone-1": [] },
        },
      ],
    });
    expect(data.content).toEqual([{ type: "Columns", props: { id: "Columns-1", layout: "1/2+1/2" } }]);
    expect(data.zones).toEqual({
      "Columns-1:zone-0": [{ type: "Cta", props: { id: "Cta-1", heading: "Hi" } }],
      "Columns-1:zone-1": [],
    });
  });

  it("gives a component stored without an id one Puck can track", () => {
    const data = storedToPuck({ content: [{ type: "Hero", props: { title: "x" } }] });
    expect(data.content[0].props.id).toMatch(/^Hero-/);
  });

  it("stores the page root flat, from either shape, keeping declared root fields only", () => {
    // Puck 0.20 keeps root values under root.props.
    const data = storedToPuck({ root: { title: "T", footer_cta_mode: "hide", stale: 1 }, content: [] });
    expect(data.root).toEqual({ props: { title: "T", footer_cta_mode: "hide", stale: 1 } });
    expect(puckToStored(data, ["footer_cta_mode"]).root).toEqual({ title: "T", footer_cta_mode: "hide" });
    // Without the declared names nothing is dropped.
    expect(puckToStored(data).root).toEqual({ title: "T", footer_cta_mode: "hide", stale: 1 });
    // An older Puck's flat root, and a page stored with root.props, read the same.
    expect(
      puckToStored({ root: { title: "Old", footer_cta_mode: "custom" }, content: [] }, ["footer_cta_mode"]).root,
    ).toEqual({ title: "Old", footer_cta_mode: "custom" });
    expect(storedToPuck({ root: { props: { title: "N", footer_cta_mode: "hide" } } }).root).toEqual({
      props: { title: "N", footer_cta_mode: "hide" },
    });
  });

  it("round-trips the v0.106 page with root fields declared", () => {
    const page = fixture("standard-page-v0.106.json");
    expect(puckToStored(storedToPuck(page), ["footer_cta_mode"])).toEqual(page);
  });

  it("reads an empty or broken hidden input as an empty page", () => {
    expect(parseStored("")).toEqual({ root: { title: "" }, content: [] });
    expect(parseStored("{not json")).toEqual({ root: { title: "" }, content: [] });
  });
});

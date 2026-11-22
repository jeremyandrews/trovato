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

  it("reads an empty or broken hidden input as an empty page", () => {
    expect(parseStored("")).toEqual({ root: { title: "" }, content: [] });
    expect(parseStored("{not json")).toEqual({ root: { title: "" }, content: [] });
  });
});

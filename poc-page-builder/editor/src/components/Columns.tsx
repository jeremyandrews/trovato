import type { ComponentConfig } from "@measured/puck";
import { DropZone } from "@measured/puck";

export interface ColumnsProps {
  layout: "1/2+1/2" | "2/3+1/3" | "1/3+2/3" | "1/3+1/3+1/3";
  gap?: string;
}

function layoutClass(layout: string): string {
  return layout.replace(/\//g, "-").replace(/\+/g, "-");
}

function zoneCount(layout: string): number {
  return layout.split("+").length;
}

export const Columns: ComponentConfig<ColumnsProps> = {
  fields: {
    layout: {
      type: "select",
      options: [
        { label: "1/2 + 1/2", value: "1/2+1/2" },
        { label: "2/3 + 1/3", value: "2/3+1/3" },
        { label: "1/3 + 2/3", value: "1/3+2/3" },
        { label: "1/3 + 1/3 + 1/3", value: "1/3+1/3+1/3" },
      ],
    },
    gap: { type: "text" },
  },
  defaultProps: {
    layout: "1/2+1/2",
    gap: "1.5rem",
  },
  render: ({ layout, gap }) => {
    const count = zoneCount(layout);
    return (
      <div
        className={`pb-columns pb-columns--${layoutClass(layout)}`}
        style={{ gap: gap || "1.5rem" }}
      >
        {Array.from({ length: count }, (_, i) => (
          <div className="pb-columns__zone" key={i}>
            <DropZone zone={`zone-${i}`} />
          </div>
        ))}
      </div>
    );
  },
};

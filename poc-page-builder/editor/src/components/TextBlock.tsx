import type { ComponentConfig } from "@measured/puck";
import Markdown from "react-markdown";

export interface TextBlockProps {
  content: string;
}

export const TextBlock: ComponentConfig<TextBlockProps> = {
  fields: {
    content: { type: "textarea" },
  },
  defaultProps: {
    content: "Write your **Markdown** content here.",
  },
  render: ({ content }) => (
    <div className="pb-text-block">
      <Markdown>{content}</Markdown>
    </div>
  ),
};

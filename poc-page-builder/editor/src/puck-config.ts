import type { Config } from "@measured/puck";
import { Hero, type HeroProps } from "./components/Hero";
import { TextBlock, type TextBlockProps } from "./components/TextBlock";
import { Columns, type ColumnsProps } from "./components/Columns";

type Components = {
  Hero: HeroProps;
  TextBlock: TextBlockProps;
  Columns: ColumnsProps;
};

export const config: Config<Components> = {
  components: {
    Hero,
    TextBlock,
    Columns,
  },
};

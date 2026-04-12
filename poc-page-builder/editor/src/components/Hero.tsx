import type { ComponentConfig } from "@measured/puck";

export interface HeroProps {
  title: string;
  subtitle?: string;
  backgroundImage?: string;
  ctaText?: string;
  ctaUrl?: string;
  variant: "standard" | "split" | "minimal";
}

export const Hero: ComponentConfig<HeroProps> = {
  fields: {
    title: { type: "text" },
    subtitle: { type: "text" },
    backgroundImage: { type: "text" },
    ctaText: { type: "text" },
    ctaUrl: { type: "text" },
    variant: {
      type: "select",
      options: [
        { label: "Standard", value: "standard" },
        { label: "Split", value: "split" },
        { label: "Minimal", value: "minimal" },
      ],
    },
  },
  defaultProps: {
    title: "Hero Title",
    variant: "standard",
  },
  render: ({ title, subtitle, backgroundImage, ctaText, ctaUrl, variant }) => {
    const style =
      backgroundImage && variant === "standard"
        ? { backgroundImage: `url('${backgroundImage}')` }
        : undefined;

    return (
      <section className={`pb-hero pb-hero--${variant}`} style={style}>
        <div className="pb-hero__content">
          <h1 className="pb-hero__title">{title}</h1>
          {subtitle && <p className="pb-hero__subtitle">{subtitle}</p>}
          {ctaText && ctaUrl && (
            <a className="pb-hero__cta" href={ctaUrl}>
              {ctaText}
            </a>
          )}
        </div>
      </section>
    );
  },
};

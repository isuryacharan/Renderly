// Icons from the design system's set (src/icons.ts). An icon is decoration unless it has a label;
// an icon-only button always has a tooltip that names its action.

import { createElement, type ButtonHTMLAttributes } from "react";
import { ICONS, type IconName } from "../icons";

export type { IconName };
import { Tooltip } from "./overlays";

export interface IconProps {
  name: IconName;
  /** Only for an icon that carries meaning on its own; most icons sit next to words. */
  label?: string;
}

export function Icon({ name, label }: IconProps) {
  return (
    <svg
      className="r-icon"
      viewBox="0 0 24 24"
      focusable="false"
      {...(label ? { role: "img", "aria-label": label } : { "aria-hidden": true })}
    >
      {ICONS[name].map(([tag, attributes], i) => createElement(tag, { key: i, ...attributes }))}
    </svg>
  );
}

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  icon: IconName;
  /** Names the action: the button's accessible name and its tooltip. */
  label: string;
  shortcut?: string;
  size?: "sm" | "md";
}

export function IconButton({
  icon,
  label,
  shortcut,
  size = "md",
  className,
  type,
  ...rest
}: IconButtonProps) {
  const classes = ["r-button", "r-button--icon"];
  if (size === "sm") classes.push("r-button--sm");
  if (className) classes.push(className);
  return (
    <Tooltip label={label} {...(shortcut ? { shortcut } : {})}>
      <button type={type ?? "button"} aria-label={label} className={classes.join(" ")} {...rest}>
        <Icon name={icon} />
      </button>
    </Tooltip>
  );
}

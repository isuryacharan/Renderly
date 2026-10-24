import { BUILTIN_TEXT_STYLES, builtinStyleRef, styleItemId, styleLabel } from "../lib/text";

/// Picker of the text styles (the four builtin caption styles), by style ref
/// (`builtin:<id>@<v>`). `value` is the current ref, or "" when the clip inherits its track's
/// style. With `onInherit` the gallery offers an extra "Track style" choice.
export function CaptionStyleGallery({
  value,
  onChange,
  onInherit,
  disabled = false,
  label = "Text styles",
}: {
  value: string;
  onChange: (styleRef: string) => void;
  onInherit?: () => void;
  disabled?: boolean;
  label?: string;
}) {
  const currentId = value ? styleItemId(value) : "";
  const known = BUILTIN_TEXT_STYLES.some((s) => s.id === currentId);
  return (
    <div className="style-gallery" role="listbox" aria-label={label}>
      {onInherit && (
        <button
          type="button"
          role="option"
          aria-selected={value === ""}
          disabled={disabled}
          className={`style-card style-inherit${value === "" ? " selected" : ""}`}
          onClick={onInherit}
        >
          <span className="style-preview">Aa</span>
          <span className="style-label">Track style</span>
        </button>
      )}
      {BUILTIN_TEXT_STYLES.map((style) => (
        <button
          key={style.id}
          type="button"
          role="option"
          aria-selected={currentId === style.id}
          disabled={disabled}
          className={`style-card style-${style.id}${currentId === style.id ? " selected" : ""}`}
          onClick={() => onChange(builtinStyleRef(style.id))}
        >
          <span className="style-preview">Aa</span>
          <span className="style-label">{style.label}</span>
        </button>
      ))}
      {value !== "" && !known && (
        // A style from an installed pack: shown (selected) so the picker never lies about it.
        <button type="button" role="option" aria-selected className="style-card selected" disabled>
          <span className="style-preview">Aa</span>
          <span className="style-label">{styleLabel(value)}</span>
        </button>
      )}
    </div>
  );
}

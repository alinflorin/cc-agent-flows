import { Combobox, Option, makeStyles } from "@fluentui/react-components";

const useStyles = makeStyles({ mono: { fontFamily: "monospace" } });

/** A text field that also offers references (`${var:NAME}`) from a list: pick one or type a fixed value. */
export function RefCombobox({ value, options, placeholder, onChange }: { value: string; options: string[]; placeholder?: string; onChange: (v: string) => void }) {
  const styles = useStyles();
  return (
    <Combobox
      freeform
      style={{ minWidth: 0 }}
      input={{ className: styles.mono }}
      placeholder={placeholder}
      value={value}
      selectedOptions={[value]}
      onChange={(e) => onChange(e.target.value)}
      onOptionSelect={(_, d) => onChange(d.optionValue ?? "")}
    >
      {options.map((o) => (
        <Option key={o} value={o}>
          {o}
        </Option>
      ))}
    </Combobox>
  );
}

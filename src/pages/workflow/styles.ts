import { makeStyles, tokens } from "@fluentui/react-components";

export const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", gap: tokens.spacingVerticalM },
  bar: { display: "flex", gap: tokens.spacingHorizontalS, flexWrap: "wrap", alignItems: "center" },
  grow: { flex: 1 },
  body: { display: "flex", gap: tokens.spacingHorizontalM, height: "min(640px, calc(100vh - 340px))", minHeight: "420px" },
  canvas: { flex: 1, minWidth: 0, border: `1px solid ${tokens.colorNeutralStroke2}`, borderRadius: tokens.borderRadiusMedium },
  side: {
    width: "300px",
    overflow: "auto",
    display: "flex",
    flexDirection: "column",
    gap: tokens.spacingVerticalM,
    padding: tokens.spacingHorizontalM,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
    "& > *": { flexShrink: 0 },
  },
  node: {
    overflowWrap: "anywhere",
    minWidth: "150px",
    maxWidth: "220px",
    padding: `${tokens.spacingVerticalS} ${tokens.spacingHorizontalM}`,
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderLeft: `4px solid ${tokens.colorBrandBackground}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  human: { borderLeft: `4px solid ${tokens.colorPaletteMarigoldBorderActive}` },
  trig: { borderLeft: `4px solid ${tokens.colorPaletteGreenBorderActive}` },
  row: { display: "flex", gap: tokens.spacingHorizontalS, alignItems: "center" },
  mono: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase200, overflowWrap: "anywhere" },
  selected: { border: `2px solid ${tokens.colorBrandStroke1}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
  warn: { border: `1px solid ${tokens.colorPaletteYellowBorder2}`, borderLeft: `4px solid ${tokens.colorBrandBackground}` },
});


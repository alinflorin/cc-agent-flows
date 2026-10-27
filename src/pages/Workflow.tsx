import { useTranslation } from "react-i18next";
import { Body1, Title2 } from "@fluentui/react-components";
import { inTauri } from "../prefs";
import { WorkflowEditor } from "./WorkflowEditor";

export function Workflow() {
  const { t } = useTranslation();
  return (
    <>
      <Title2 as="h1" block>
        {t("nav.workflow")}
      </Title2>
      <Body1 block style={{ margin: "8px 0 16px" }}>
        {t("wf.intro")}
      </Body1>
      {inTauri() ? <WorkflowEditor /> : <Body1 block>{t("app.desktopOnly")}</Body1>}
    </>
  );
}

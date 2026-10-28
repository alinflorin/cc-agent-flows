import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { Body1 } from "@fluentui/react-components";
import { inTauri } from "../prefs";
import { WorkflowEditor } from "./WorkflowEditor";

export function Workflow() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("nav.workflow")} description={t("wf.intro")} />
      {inTauri() ? <WorkflowEditor /> : <Body1 block>{t("app.desktopOnly")}</Body1>}
    </>
  );
}

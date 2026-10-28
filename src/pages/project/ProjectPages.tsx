import { useTranslation } from "react-i18next";
import { PageHeader } from "../../components/PageHeader";
import { ImagesSettings } from "../settings/Images";
import { InjectionsSettings } from "../settings/Injections";
import { SecretsSettings } from "../settings/Secrets";
import { VariablesSettings } from "../settings/Variables";

// The project's images, variables, secrets and injections are the same editors as in the global
// settings; inside a project they work on the project's own lists (see `useProject`).

export function ProjectImages() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("settings.tabs.images")} />
      <ImagesSettings />
    </>
  );
}

export function ProjectVariables() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("settings.tabs.variables")} />
      <VariablesSettings />
    </>
  );
}

export function ProjectSecrets() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("settings.tabs.secrets")} />
      <SecretsSettings />
    </>
  );
}

export function ProjectInjections() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("settings.tabs.injections")} />
      <InjectionsSettings />
    </>
  );
}

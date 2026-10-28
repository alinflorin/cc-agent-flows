import { PageHeader } from "../components/PageHeader";
import { useTranslation } from "react-i18next";
import { getPref } from "../prefs";

export function Home() {
  const { t } = useTranslation();
  const name = getPref<string>("displayName");
  return (
    <>
      <PageHeader title={name ? t("home.greeting", { name }) : t("home.welcome")} description={t("home.intro")} />
    </>
  );
}

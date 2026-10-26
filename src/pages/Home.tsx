import { useTranslation } from "react-i18next";
import { Body1, Title2 } from "@fluentui/react-components";
import { getPref } from "../prefs";

export function Home() {
  const { t } = useTranslation();
  const name = getPref<string>("displayName");
  return (
    <>
      <Title2 as="h1" block>
        {name ? t("home.greeting", { name }) : t("home.welcome")}
      </Title2>
      <Body1 block>{t("home.intro")}</Body1>
    </>
  );
}

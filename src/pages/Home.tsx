import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { Body1, Card, CardHeader, makeStyles, tokens } from "@fluentui/react-components";
import { Bot24Regular, Chat24Regular, Flow24Regular } from "@fluentui/react-icons";
import { PageHeader } from "../components/PageHeader";

const useStyles = makeStyles({
  grid: { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))", gap: tokens.spacingHorizontalM },
  icon: { color: tokens.colorBrandForeground1 },
  desc: { color: tokens.colorNeutralForeground3 },
});

export function Home() {
  const { t } = useTranslation();
  const styles = useStyles();
  const navigate = useNavigate();
  const cards = [
    { to: "/sessions", key: "sessions", title: t("nav.sessions"), icon: <Chat24Regular /> },
    { to: "/workflow", key: "workflow", title: t("nav.workflow"), icon: <Flow24Regular /> },
    { to: "/profiles", key: "profiles", title: t("nav.profiles"), icon: <Bot24Regular /> },
  ];
  return (
    <>
      <PageHeader title={t("home.welcome")} description={t("home.intro")} />
      <div className={styles.grid}>
        {cards.map((c) => (
          <Card key={c.to} onClick={() => navigate(c.to)}>
            <CardHeader
              image={<span className={styles.icon}>{c.icon}</span>}
              header={<Body1><b>{c.title}</b></Body1>}
              description={<span className={styles.desc}>{t(`home.${c.key}`)}</span>}
            />
          </Card>
        ))}
      </div>
    </>
  );
}

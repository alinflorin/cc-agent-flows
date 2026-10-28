import { createContext, useContext, useEffect, useState } from "react";
import { Outlet, useParams } from "react-router";
import { useTranslation } from "react-i18next";
import { Body1 } from "@fluentui/react-components";
import { inTauri } from "./prefs";
import { call } from "./tauri";

/** The id of the demo project the welcome dialog creates (`demo/demo.yaml`). */
export const DEMO_PROJECT = "demo";

export interface ProjectInfo {
  id: string;
  name: string;
  description: string;
  system_prompt: string;
  allow_hosts: string[];
  workflows: number;
  sessions: number;
  profiles: number;
}

/** The project a page works in; `null` in the global settings. Settings pages that exist in both read it to pick the scope of their commands. */
const ProjectContext = createContext<string | null>(null);

export const useProject = () => useContext(ProjectContext);

/** For the pages that only exist inside a project. */
export function useProjectId(): string {
  return useProject() ?? "";
}

/** Route element of `/p/:pid/*`: checks the project exists and provides it to the pages below. */
export function ProjectRoute() {
  const { t } = useTranslation();
  const { pid = "" } = useParams();
  const [known, setKnown] = useState<boolean | null>(inTauri() ? null : true);

  useEffect(() => {
    if (!inTauri()) return;
    setKnown(null);
    call("projects_get", { id: pid }).then(
      () => setKnown(true),
      () => setKnown(false),
    );
  }, [pid]);

  if (known === false) return <Body1 block>{t("projects.missing")}</Body1>;
  if (known === null) return null;
  // a key per project, so nothing of the previous one is kept on screen
  return (
    <ProjectContext.Provider key={pid} value={pid}>
      <Outlet />
    </ProjectContext.Provider>
  );
}

/** A link target inside the current project. */
export const projectPath = (pid: string, rest = "") => `/p/${pid}${rest}`;

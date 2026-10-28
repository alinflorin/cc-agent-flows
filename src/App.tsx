import { Suspense } from "react";
import { Navigate, createHashRouter, RouterProvider } from "react-router";
import { FluentProvider, makeStaticStyles } from "@fluentui/react-components";
import { Layout } from "./components/Layout";
import { NotificationsProvider } from "./notifications";
import { About } from "./pages/About";
import { Analytics } from "./pages/Analytics";
import { ProjectImages, ProjectInjections, ProjectSecrets, ProjectVariables } from "./pages/project/ProjectPages";
import { ProjectSettings } from "./pages/project/ProjectSettings";
import { Projects } from "./pages/Projects";
import { Profiles } from "./pages/Profiles";
import { Runs } from "./pages/Runs";
import { SessionView } from "./pages/session/SessionView";
import { Sessions } from "./pages/Sessions";
import { Settings } from "./pages/Settings";
import { Workflow } from "./pages/workflow/Workflow";
import { ProjectRoute } from "./project";
import { useFluentTheme } from "./theme";
import { UpdatesProvider } from "./updates";

// Buttons and fields keep their height in a column that overflows (they would be squeezed until the
// label is taller than the button); long words wrap instead of leaving their box.
const useGlobalStyles = makeStaticStyles({
  ".fui-Button, .fui-ToggleButton, .fui-Field, .fui-Switch, .fui-Checkbox": { flexShrink: 0 },
  body: { overflowWrap: "anywhere" },
});

// Hash routes: the packaged app serves static files, so deep links can't rely on a server fallback.
const router = createHashRouter([
  {
    element: <Layout />,
    children: [
      { path: "/", element: <Projects /> },
      {
        path: "/p/:pid",
        element: <ProjectRoute />,
        children: [
          { index: true, element: <Navigate to="sessions" replace /> },
          { path: "workflow", element: <Workflow /> },
          { path: "sessions", element: <Sessions /> },
          { path: "sessions/:sid", element: <SessionView /> },
          { path: "profiles", element: <Profiles /> },
          { path: "runs", element: <Runs /> },
          { path: "analytics", element: <Analytics /> },
          { path: "images", element: <ProjectImages /> },
          { path: "variables", element: <ProjectVariables /> },
          { path: "secrets", element: <ProjectSecrets /> },
          { path: "injections", element: <ProjectInjections /> },
          { path: "settings", element: <ProjectSettings /> },
        ],
      },
      { path: "/about", element: <About /> },
      { path: "/settings", element: <Navigate to="general" replace /> },
      { path: "/settings/:tab", element: <Settings /> },
    ],
  },
]);

export default function App() {
  useGlobalStyles();
  const { theme, dark } = useFluentTheme();
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
  return (
    <FluentProvider theme={theme} style={{ height: "100%" }}>
      <NotificationsProvider>
        <UpdatesProvider>
          <Suspense fallback={null}>
            <RouterProvider router={router} />
          </Suspense>
        </UpdatesProvider>
      </NotificationsProvider>
    </FluentProvider>
  );
}

import { Suspense } from "react";
import { createHashRouter, RouterProvider } from "react-router";
import { FluentProvider } from "@fluentui/react-components";
import { Layout } from "./components/Layout";
import { NotificationsProvider } from "./notifications";
import { About } from "./pages/About";
import { Home } from "./pages/Home";
import { Settings } from "./pages/Settings";
import { useFluentTheme } from "./theme";
import { UpdatesProvider } from "./updates";

// Hash routes: the packaged app serves static files, so deep links can't rely on a server fallback.
const router = createHashRouter([
  {
    element: <Layout />,
    children: [
      { path: "/", element: <Home /> },
      { path: "/about", element: <About /> },
      { path: "/settings", element: <Settings /> },
    ],
  },
]);

export default function App() {
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

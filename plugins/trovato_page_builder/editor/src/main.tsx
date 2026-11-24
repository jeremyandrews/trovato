/**
 * Mount the page builder on every `[data-page-builder]` widget a form holds.
 *
 * The widget (templates/form/widget--page_builder.html) carries a hidden
 * input with the stored page JSON and the URL of the component registry.
 * The editor reads the registry, builds its config from it, opens the stored
 * page, and writes the page back into the hidden input on every change, so
 * the form's own Save button saves it and the kernel validates it.
 */

import { Puck } from "@measured/puck";
import "@measured/puck/no-external.css";
import React from "react";
import { createRoot } from "react-dom/client";

import { parseStored, puckToStored, storedToPuck, type PuckData } from "./data";
import type { EditorContext } from "./fields";
import { buildConfig } from "./puck-config";
import type { ComponentDefinition, RegistryResponse, RootFieldDefinition } from "./registry";
import "./editor.css";

async function mount(host: HTMLElement): Promise<void> {
  const inputId = host.dataset.pageBuilderInput ?? "";
  const input = document.getElementById(inputId) as HTMLInputElement | null;
  if (!input) return;
  const registryUrl = host.dataset.componentsUrl ?? "/api/v1/page-builder/components";

  host.classList.add("pb-editor");
  host.textContent = "Loading the page builder…";

  let registry: ComponentDefinition[];
  let rootFields: RootFieldDefinition[];
  try {
    const response = await fetch(registryUrl, { credentials: "same-origin" });
    if (!response.ok) throw new Error(String(response.status));
    const body = (await response.json()) as RegistryResponse;
    registry = body.data;
    rootFields = body.root_fields ?? [];
  } catch (e) {
    host.textContent = `The page builder could not load its components (${String(e)}).`;
    return;
  }

  const ctx: EditorContext = {
    previewUrl: host.dataset.previewUrl ?? "/api/v1/page-builder/preview",
  };
  const config = buildConfig(registry, ctx, rootFields);
  const rootNames = rootFields.map((f) => f.name);
  const initial = storedToPuck(parseStored(input.value));
  // Ids generated for components stored without one are part of the page.
  input.value = JSON.stringify(puckToStored(initial, rootNames));

  host.textContent = "";
  createRoot(host).render(
    <Puck
      config={config}
      data={initial}
      iframe={{ enabled: false }}
      onChange={(data) => {
        input.value = JSON.stringify(puckToStored(data as unknown as PuckData, rootNames));
      }}
      headerTitle="Page builder"
      overrides={{
        // The form's own Save button saves the page; Puck's Publish is not used.
        headerActions: () => <span className="pb-editor__hint">Saved with the form</span>,
      }}
    />,
  );
}

function mountAll(): void {
  document.querySelectorAll<HTMLElement>("[data-page-builder]").forEach((host) => {
    if (host.dataset.pageBuilderMounted) return;
    host.dataset.pageBuilderMounted = "1";
    void mount(host);
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", mountAll);
} else {
  mountAll();
}

import { useState, useRef } from "react";
import { Puck, type Data } from "@measured/puck";
import "@measured/puck/puck.css";
import { config } from "./puck-config";

const initialData: Data = {
  root: { props: { title: "" } },
  content: [],
  zones: {},
};

export default function App() {
  const [exportedJson, setExportedJson] = useState<string>("");
  const [importText, setImportText] = useState<string>("");
  const [data, setData] = useState<Data>(initialData);
  const [key, setKey] = useState(0); // force re-mount on import
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleExport = (currentData: Data) => {
    const json = JSON.stringify(currentData, null, 2);
    setExportedJson(json);
    console.log("Puck JSON export:", json);
  };

  const handleImport = () => {
    try {
      const parsed = JSON.parse(importText);
      setData(parsed);
      setKey((k) => k + 1);
      setImportText("");
    } catch {
      alert("Invalid JSON");
    }
  };

  return (
    <div>
      <Puck
        key={key}
        config={config}
        data={data}
        onPublish={handleExport}
        onChange={setData}
      />

      <div style={{ padding: "1rem", borderTop: "2px solid #e5e7eb" }}>
        <h2 style={{ margin: "0 0 1rem" }}>JSON Import / Export</h2>

        <div style={{ display: "flex", gap: "1rem", marginBottom: "1rem" }}>
          <button
            onClick={() => handleExport(data)}
            style={{ padding: "0.5rem 1rem", cursor: "pointer" }}
          >
            Export JSON
          </button>
        </div>

        {exportedJson && (
          <details open style={{ marginBottom: "1rem" }}>
            <summary style={{ cursor: "pointer", fontWeight: 600 }}>
              Exported JSON ({exportedJson.length} chars)
            </summary>
            <pre
              style={{
                background: "#f1f5f9",
                padding: "1rem",
                borderRadius: "0.375rem",
                overflow: "auto",
                maxHeight: "400px",
                fontSize: "0.8rem",
              }}
            >
              {exportedJson}
            </pre>
          </details>
        )}

        <div style={{ marginTop: "1rem" }}>
          <label
            htmlFor="import-json"
            style={{ display: "block", fontWeight: 600, marginBottom: "0.5rem" }}
          >
            Load JSON:
          </label>
          <textarea
            id="import-json"
            ref={textareaRef}
            value={importText}
            onChange={(e) => setImportText(e.target.value)}
            placeholder="Paste Puck JSON here..."
            rows={6}
            style={{
              width: "100%",
              fontFamily: "monospace",
              fontSize: "0.8rem",
              padding: "0.5rem",
            }}
          />
          <button
            onClick={handleImport}
            disabled={!importText.trim()}
            style={{ marginTop: "0.5rem", padding: "0.5rem 1rem", cursor: "pointer" }}
          >
            Load JSON
          </button>
        </div>
      </div>
    </div>
  );
}

import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Repo } from "../types";

export function Settings({
  repos,
  onAdd,
  onRename,
  onDelete,
}: {
  repos: Repo[];
  onAdd: (name: string, path: string, project: string | null) => void;
  onRename: (id: number, name: string) => void;
  onDelete: (id: number) => void;
}) {
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [project, setProject] = useState("");
  const [editId, setEditId] = useState<number | null>(null);
  const [editVal, setEditVal] = useState("");

  const commitRename = (r: Repo) => {
    const v = editVal.trim();
    if (v && v !== r.name) onRename(r.id, v);
    setEditId(null);
  };

  const pick = async () => {
    const sel = await open({ directory: true, multiple: false, title: "Choose a git repository" });
    if (typeof sel === "string") {
      setPath(sel);
      if (!name) setName(sel.split("/").filter(Boolean).pop() ?? "");
    }
  };

  const add = () => {
    onAdd(name.trim(), path, project.trim() || null);
    setName("");
    setPath("");
    setProject("");
  };

  return (
    <div className="sessions settings">
      <h3>Repositories</h3>
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Path</th>
            <th>Default Jira project</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {repos.length === 0 && (
            <tr>
              <td className="empty" colSpan={4}>
                No repos yet — add one below to start working tickets.
              </td>
            </tr>
          )}
          {repos.map((r) => (
            <tr key={r.id}>
              <td>
                {editId === r.id ? (
                  <input
                    autoFocus
                    value={editVal}
                    onChange={(e) => setEditVal(e.target.value)}
                    onBlur={() => commitRename(r)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename(r);
                      if (e.key === "Escape") setEditId(null);
                    }}
                  />
                ) : (
                  <span
                    className="renamable"
                    title="Click to rename"
                    onClick={() => {
                      setEditId(r.id);
                      setEditVal(r.name);
                    }}
                  >
                    {r.name}
                  </span>
                )}
              </td>
              <td className="mono path">{r.path}</td>
              <td className="mono">{r.default_project_key ?? "—"}</td>
              <td>
                <button className="row-del" title="Remove repo" onClick={() => onDelete(r.id)}>
                  ×
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3>Add a repository</h3>
      <div className="settings-add">
        <input placeholder="name" value={name} onChange={(e) => setName(e.target.value)} />
        <button onClick={pick}>{path || "Choose folder…"}</button>
        <input
          placeholder="default Jira project key (optional, e.g. DNA)"
          value={project}
          onChange={(e) => setProject(e.target.value)}
        />
        <button disabled={!name.trim() || !path} onClick={add}>
          Add repo
        </button>
      </div>
    </div>
  );
}

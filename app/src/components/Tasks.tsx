type Todo = { content: string; status: string };

export function Tasks({ todosJson }: { todosJson: string }) {
  let todos: Todo[] = [];
  try {
    todos = JSON.parse(todosJson || "[]");
  } catch {
    todos = [];
  }
  if (!todos.length) return null;

  const done = todos.filter((t) => t.status === "completed").length;

  return (
    <div className="tasks">
      <div className="tasks-head">
        Claude tasks <span className="muted">{done}/{todos.length}</span>
      </div>
      <ul>
        {todos.map((t, i) => (
          <li key={i} className={"task " + t.status}>
            <span className="task-box">
              {t.status === "completed" ? "✓" : t.status === "in_progress" ? "▸" : ""}
            </span>
            <span className="task-text">{t.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

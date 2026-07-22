namespace Mvf.Hosting.Worker;

/// <summary>
/// How to launch an out-of-process module worker (local child process; no network).
/// </summary>
/// <param name="ArenaPath">
/// Backing-file path of the shared-memory arena, exported to the child as <c>MVF_ARENA_PATH</c> so it
/// can map the arena and read frames by handle. Null when the child should use the inline (base64)
/// frame path.
/// </param>
/// <param name="StartupBudget">
/// How long to wait for the child's <c>hello</c> handshake and (when it warms up asynchronously) its
/// <c>ready</c> signal before treating startup as failed. Bounds a slow model load / device connect so it
/// can't hang the engine, while being generous enough not to kill a legitimately slow warmup. Null → 30s.
/// </param>
/// <param name="Environment">Extra environment variables to export to the child (on top of the inherited
/// environment, <c>PYTHONPATH</c> and <c>MVF_ARENA_PATH</c>). Null = none.</param>
public sealed record WorkerLaunchInfo(
    string Command,
    IReadOnlyList<string> Args,
    string WorkingDirectory,
    string? PythonPath = null,
    string? ArenaPath = null,
    TimeSpan? StartupBudget = null,
    IReadOnlyDictionary<string, string>? Environment = null);

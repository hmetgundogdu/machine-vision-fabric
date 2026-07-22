namespace Mvf.Hosting.Worker;

/// <summary>
/// How to launch an out-of-process module worker (local child process; no network).
/// </summary>
/// <param name="ArenaPath">
/// Backing-file path of the shared-memory arena, exported to the child as <c>MVF_ARENA_PATH</c> so it
/// can map the arena and read frames by handle. Null when the child should use the inline (base64)
/// frame path.
/// </param>
public sealed record WorkerLaunchInfo(
    string Command,
    IReadOnlyList<string> Args,
    string WorkingDirectory,
    string? PythonPath = null,
    string? ArenaPath = null);

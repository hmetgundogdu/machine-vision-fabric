using Mvf.Abstractions;

namespace Mvf.Sdk;

/// <summary>
/// How an in-process .NET module reports what it is doing. Call these from a processor / classifier /
/// sink / source and the line appears in the CLI dashboard (the node's log panel) and, headless, on the
/// engine's stderr — the same "log reaches the upper layer" contract the Python and C++ SDKs give an
/// out-of-process module.
///
/// <para>Backed by an ambient sink the engine installs around each node call
/// (<see cref="ModuleLogContext"/>). When nothing is listening the call is a cheap no-op, so a module
/// may log unconditionally.</para>
/// </summary>
public static class ModuleLog
{
    /// <summary>Verbose detail, usually filtered out.</summary>
    public static void Debug(string message) => ModuleLogContext.Emit("debug", message);

    /// <summary>Normal progress information.</summary>
    public static void Info(string message) => ModuleLogContext.Emit("info", message);

    /// <summary>Something recoverable that the operator should notice.</summary>
    public static void Warn(string message) => ModuleLogContext.Emit("warn", message);

    /// <summary>A failure the module handled without throwing.</summary>
    public static void Error(string message) => ModuleLogContext.Emit("error", message);
}

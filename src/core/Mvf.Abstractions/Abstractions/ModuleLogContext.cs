namespace Mvf.Abstractions;

/// <summary>
/// Ambient log sink for an <b>in-process</b> module. An out-of-process worker sends log lines over the
/// stdio protocol; an in-process .NET module has no such channel, so the engine installs a per-flow
/// <see cref="Holder"/> here and the SDK's <c>ModuleLog</c> writes to it with <see cref="Emit"/>.
///
/// <para><b>Zero per-cycle allocation.</b> The engine writes the <see cref="AsyncLocal{T}"/> exactly once
/// per execution flow (the serial cycle loop, or each pipelined stage) via <see cref="Enter"/> — that one
/// write is what makes the sink flow correctly across a module's <c>await</c> hops. Thereafter the current
/// node's sink is swapped in by a plain field write (<see cref="Holder.Sink"/>), so per-node, per-cycle
/// logging allocates nothing. When no sink is installed <see cref="Emit"/> is a no-op, so a module can log
/// unconditionally with no cost on a run that ignores logs.</para>
/// </summary>
public static class ModuleLogContext
{
    private static readonly AsyncLocal<Holder?> Current = new();

    /// <summary>The per-flow slot whose <see cref="Sink"/> the engine swaps per node with no async-local write.</summary>
    public sealed class Holder
    {
        /// <summary>Where <see cref="Emit"/> routes; <c>(level, message)</c>. Null discards.</summary>
        public Action<string, string>? Sink;
    }

    /// <summary>
    /// Installs a fresh holder for the current flow (one async-local write) and returns it. Call once per
    /// flow, then set <see cref="Holder.Sink"/> per node. Pair with <see cref="Exit"/>.
    /// </summary>
    public static Holder Enter()
    {
        var holder = new Holder();
        Current.Value = holder;
        return holder;
    }

    /// <summary>Clears the current flow's holder so it cannot leak into a later run on the same host.</summary>
    public static void Exit() => Current.Value = null;

    /// <summary>Routes one log line to the current sink, if any. <paramref name="level"/> is debug/info/warn/error.</summary>
    public static void Emit(string level, string message) => Current.Value?.Sink?.Invoke(level, message);
}

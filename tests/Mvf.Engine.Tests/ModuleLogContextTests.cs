using Mvf.Abstractions;

namespace Mvf.Engine.Tests;

/// <summary>
/// The ambient sink an in-process module logs through: after <see cref="ModuleLogContext.Enter"/> the
/// holder's sink can be swapped per node with a plain field write (zero per-cycle allocation) and each
/// line routes to the current sink. <see cref="ModuleLogContext.Exit"/> clears it. With no holder
/// installed, emitting is a no-op.
/// </summary>
public sealed class ModuleLogContextTests
{
    [Fact]
    public void Emit_WithoutHolder_IsNoOp()
    {
        // Must not throw when nothing is listening (the headless-with-no-callback path).
        ModuleLogContext.Exit();
        ModuleLogContext.Emit("info", "no one is listening");
    }

    [Fact]
    public void Enter_ThenSwapSink_RoutesToCurrentSink()
    {
        var nodeA = new List<string>();
        var nodeB = new List<string>();

        var holder = ModuleLogContext.Enter();
        try
        {
            // One Enter for the flow; per node just swap the sink field — no further async-local writes.
            holder.Sink = (level, message) => nodeA.Add($"{level}:{message}");
            ModuleLogContext.Emit("info", "a1");
            ModuleLogContext.Emit("warn", "a2");

            holder.Sink = (level, message) => nodeB.Add($"{level}:{message}");
            ModuleLogContext.Emit("error", "b1");

            // A null sink drops lines without throwing (a node no one attached a sink for).
            holder.Sink = null;
            ModuleLogContext.Emit("info", "dropped");
        }
        finally
        {
            ModuleLogContext.Exit();
        }

        // After Exit the holder is gone, so emitting goes nowhere again.
        ModuleLogContext.Emit("info", "after-exit");

        Assert.Equal(["info:a1", "warn:a2"], nodeA);
        Assert.Equal(["error:b1"], nodeB);
    }
}

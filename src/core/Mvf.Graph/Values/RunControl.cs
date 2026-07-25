namespace Mvf.Graph.Values;

/// <summary>
/// The run's running state — <c>running</c> or <c>paused</c> — flipped from outside the executor (a CLI
/// shortcut) and read by it once per iteration. It exists because the graph carries a <c>loop</c>: the
/// loop is the boundary that makes "the cycle" a first-class thing, and pausing means the loop stops
/// advancing.
///
/// <para><b>Pause is not cancel.</b> Pausing stops the loop advancing while everything stays put — state
/// preserved, workers warm, resume continues where it left off. It is cheap and reversible, the everyday
/// operator control. Cancellation (interrupting in-flight work, tearing modules down, one token per run)
/// is a different, heavier mechanism and is deliberately not this.</para>
///
/// <para>Whole-graph scope on purpose: one flag pauses the entire run, however many <c>loop</c> nodes it
/// has. Per-loop pause is a later refinement, not a first-slice concern.</para>
///
/// <para>The read is a plain volatile read on the hot path — the executor idles while paused and never
/// waits on a lock. Toggling is likewise lock-free; a torn read at worst costs one extra idle poll.</para>
/// </summary>
public sealed class RunControl
{
    private volatile bool _paused;

    public bool IsPaused => _paused;

    public bool IsRunning => !_paused;

    public void Pause() => _paused = true;

    public void Resume() => _paused = false;

    /// <summary>Flips the state and returns the new one (true = now paused).</summary>
    public bool Toggle() => _paused = !_paused;
}

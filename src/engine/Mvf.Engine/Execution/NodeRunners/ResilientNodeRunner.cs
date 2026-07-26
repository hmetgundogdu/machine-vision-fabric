using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps a node runner so a failure (a camera timing out, a sink losing its connection, a model faulting) is
/// retried per a <see cref="NodeFailurePolicy"/> instead of taking the node's default path straight away. The
/// executors call <c>ExecuteAsync</c> exactly as before — the retrying happens inside here, so neither
/// executor changes, and it works for any node (source, compute, classify, sink).
///
/// <para>Recovery is a <b>hard restart</b> when a rebuild factory is supplied (the normal path from
/// <see cref="PipelineNodeActivator"/>): the broken runner is disposed and a brand-new one is built from
/// scratch — a fresh session/model — so the node recovers even when its state is not just stalled but dead.
/// Without a factory it falls back to a soft restart that re-activates the same runner.</para>
///
/// <para>Semantics:</para>
/// <list type="bullet">
///   <item>A clean <see cref="NodeExecutionResult.NoOutput"/> passes straight through — an empty result is not
///     a failure and is never retried.</item>
///   <item>Work only ever runs against a freshly restarted runner, so a returned NoOutput always means a
///     genuine empty result, never a broken runner reporting empty.</item>
///   <item>When a bounded restart runs out of attempts (<see cref="NodeFailurePolicy.Limit"/>) the last error
///     is rethrown, so the executor handles it exactly as it would have without this wrapper — a source ends
///     the run (the honesty guarantee), a mid-graph node's cycle is skipped. An unbounded restart
///     (<c>Limit == 0</c>) keeps going until cancelled, so a run rides out an outage and resumes the moment
///     the node comes back.</item>
/// </list>
///
/// <para>Only the execution path is made resilient; the initial activation at pipeline start stays fail-fast,
/// so a node that cannot come up at all is reported immediately.</para>
/// </summary>
internal class ResilientNodeRunner(
    INodeRunner inner,
    NodeFailurePolicy policy,
    Action<string, string>? log = null,
    Func<CancellationToken, Task<INodeRunner>>? rebuild = null) : INodeRunner
{
    private INodeRunner _inner = inner;

    /// <summary>The live wrapped runner. Swapped out on a hard restart; the rewindable subclass reads it.</summary>
    protected INodeRunner Inner => _inner;

    public string NodeId => _inner.NodeId;

    // Initial activation is deliberately not retried — a node that never comes up at start is reported now.
    public Task ActivateAsync(CancellationToken cancellationToken) => _inner.ActivateAsync(cancellationToken);

    public ValueTask DisposeAsync() => _inner.DisposeAsync();

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        // One shared attempt budget for the whole call: every failure and every failed restart counts against
        // it, so a bounded restart can never loop forever. Reset per call, so each execution gets a fresh
        // budget rather than the run draining one over its lifetime.
        var attempt = 0;

        while (true)
        {
            try
            {
                return await _inner.ExecuteAsync(inputs, cancellationToken);
            }
            catch (Exception failure) when (failure is not OperationCanceledException)
            {
                var lastError = failure;
                while (true)
                {
                    attempt++;
                    if (!policy.AllowsAttempt(attempt))
                    {
                        log?.Invoke("error", $"node failed after {attempt - 1} restart attempt(s): {lastError.Message}");
                        throw lastError;
                    }

                    var backoff = policy.BackoffFor(attempt);
                    var bound   = policy.Limit > 0 ? $"/{policy.Limit}" : string.Empty;
                    log?.Invoke("warn", $"node error (attempt {attempt}{bound}), restarting in {backoff.TotalMilliseconds:F0}ms: {lastError.Message}");

                    await Task.Delay(backoff, cancellationToken);

                    try
                    {
                        await RestartAsync(cancellationToken);
                        break; // restarted — run again
                    }
                    catch (Exception restartError) when (restartError is not OperationCanceledException)
                    {
                        lastError = restartError; // a failed restart is itself an attempt; back off and try again
                    }
                }
            }
        }
    }

    /// <summary>
    /// Brings the node back. With a rebuild factory this is a hard restart: build a fresh runner (a new
    /// session/model), activate it, then swap it in and dispose the broken one. Without a factory it
    /// re-activates the same runner.
    /// </summary>
    private async Task RestartAsync(CancellationToken cancellationToken)
    {
        if (rebuild is null)
        {
            await _inner.ActivateAsync(cancellationToken);
            return;
        }

        var fresh = await rebuild(cancellationToken);
        try
        {
            await fresh.ActivateAsync(cancellationToken);
        }
        catch
        {
            await DisposeQuietlyAsync(fresh); // do not leak the half-built runner; the attempt still counts
            throw;
        }

        var broken = _inner;
        _inner = fresh;
        await DisposeQuietlyAsync(broken);
    }

    private static async ValueTask DisposeQuietlyAsync(INodeRunner runner)
    {
        try { await runner.DisposeAsync(); }
        catch { /* best effort — a broken runner may not dispose cleanly */ }
    }
}

/// <summary>
/// The <see cref="ResilientNodeRunner"/> variant used when the wrapped runner can rewind (a <c>loop</c>'s
/// <c>forever</c> replays a finite source through <see cref="IRewindableSource"/>). Kept as a separate type so
/// <c>is IRewindableSource</c> reflects the inner runner's real capability rather than always claiming it.
/// </summary>
internal sealed class RewindableResilientNodeRunner(
    INodeRunner inner,
    NodeFailurePolicy policy,
    Action<string, string>? log = null,
    Func<CancellationToken, Task<INodeRunner>>? rebuild = null)
    : ResilientNodeRunner(inner, policy, log, rebuild), IRewindableSource
{
    public Task RewindAsync(CancellationToken cancellationToken) =>
        Inner is IRewindableSource rewindable
            ? rewindable.RewindAsync(cancellationToken)
            : Inner.ActivateAsync(cancellationToken);
}

/// <summary>Wraps a node runner for resilience, preserving <see cref="IRewindableSource"/> when the inner runner has it.</summary>
internal static class ResilientNodeRunnerFactory
{
    public static INodeRunner Wrap(
        INodeRunner inner,
        NodeFailurePolicy policy,
        Action<string, string>? log,
        Func<CancellationToken, Task<INodeRunner>>? rebuild = null) =>
        inner is IRewindableSource
            ? new RewindableResilientNodeRunner(inner, policy, log, rebuild)
            : new ResilientNodeRunner(inner, policy, log, rebuild);
}

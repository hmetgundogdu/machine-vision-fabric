using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps a source runner so a mid-stream read failure (a camera timing out, a dropped connection) is retried
/// per a <see cref="SourceFailurePolicy"/> instead of ending the whole run. The executors call
/// <c>ExecuteAsync</c> exactly as before — the retrying happens inside here, so neither executor changes.
///
/// <para>Recovery is a <b>hard restart</b> when a rebuild factory is supplied (the normal path from
/// <see cref="PipelineNodeActivator"/>): the broken runner is disposed and a brand-new one is built from
/// scratch — a fresh <c>OpenSession</c> — so the node recovers even when its session is not just stalled but
/// dead. Without a factory it falls back to a soft reconnect that re-opens the same runner's stream.</para>
///
/// <para>Semantics:</para>
/// <list type="bullet">
///   <item>A clean <see cref="NodeExecutionResult.NoOutput"/> (end of stream) passes straight through — an
///     exhausted source is not a failure and is never retried.</item>
///   <item>A read only ever runs against a freshly restarted runner, so a returned NoOutput always means a
///     genuine end of stream, never a broken reader reporting empty.</item>
///   <item>When a bounded restart runs out of attempts (<see cref="SourceFailurePolicy.Limit"/>) the last
///     error is rethrown, so the executor fails the run exactly as it would have without this wrapper — the
///     honesty guarantee is preserved. An unbounded restart (<c>Limit == 0</c>) keeps going until cancelled,
///     so a run rides out an outage and resumes the moment the source comes back.</item>
/// </list>
///
/// <para>Only the read path is made resilient; the initial activation at pipeline start stays fail-fast, so a
/// source that is entirely absent when the run begins still reports that immediately.</para>
/// </summary>
internal class ResilientSourceRunner(
    INodeRunner inner,
    SourceFailurePolicy policy,
    Action<string, string>? log = null,
    Func<CancellationToken, Task<INodeRunner>>? rebuild = null) : INodeRunner
{
    private INodeRunner _inner = inner;

    /// <summary>The live wrapped runner. Swapped out on a hard restart; the rewindable subclass reads it.</summary>
    protected INodeRunner Inner => _inner;

    public string NodeId => _inner.NodeId;

    // Initial activation is deliberately not retried — a source that never comes up at start is reported now.
    public Task ActivateAsync(CancellationToken cancellationToken) => _inner.ActivateAsync(cancellationToken);

    public ValueTask DisposeAsync() => _inner.DisposeAsync();

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        // One shared attempt budget for the whole frame-pull: every read failure and every failed restart
        // counts against it, so a bounded Retry can never loop forever. Reset per pull, so each frame gets a
        // fresh budget rather than the run draining one over its lifetime.
        var attempt = 0;

        while (true)
        {
            try
            {
                return await _inner.ExecuteAsync(inputs, cancellationToken);
            }
            catch (Exception readError) when (readError is not OperationCanceledException)
            {
                var lastError = readError;
                while (true)
                {
                    attempt++;
                    if (!policy.AllowsAttempt(attempt))
                    {
                        log?.Invoke("error", $"source failed after {attempt - 1} restart attempt(s): {lastError.Message}");
                        throw lastError;
                    }

                    var backoff = policy.BackoffFor(attempt);
                    var bound   = policy.Limit > 0 ? $"/{policy.Limit}" : string.Empty;
                    log?.Invoke("warn", $"source error (attempt {attempt}{bound}), restarting in {backoff.TotalMilliseconds:F0}ms: {lastError.Message}");

                    await Task.Delay(backoff, cancellationToken);

                    try
                    {
                        await RestartAsync(cancellationToken);
                        break; // restarted — read again
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
    /// Brings the source back. With a rebuild factory this is a hard restart: build a fresh runner (a new
    /// <c>OpenSession</c>), activate it, then swap it in and dispose the broken one. Without a factory it
    /// re-opens the same runner's stream.
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
/// The <see cref="ResilientSourceRunner"/> variant used when the wrapped source can rewind (a <c>loop</c>'s
/// <c>forever</c> replays a finite source through <see cref="IRewindableSource"/>). Kept as a separate type
/// so <c>is IRewindableSource</c> reflects the inner runner's real capability rather than always claiming it.
/// </summary>
internal sealed class RewindableResilientSourceRunner(
    INodeRunner inner,
    SourceFailurePolicy policy,
    Action<string, string>? log = null,
    Func<CancellationToken, Task<INodeRunner>>? rebuild = null)
    : ResilientSourceRunner(inner, policy, log, rebuild), IRewindableSource
{
    public Task RewindAsync(CancellationToken cancellationToken) =>
        Inner is IRewindableSource rewindable
            ? rewindable.RewindAsync(cancellationToken)
            : Inner.ActivateAsync(cancellationToken);
}

/// <summary>Wraps a source runner for resilience, preserving <see cref="IRewindableSource"/> when the inner runner has it.</summary>
internal static class ResilientSourceRunnerFactory
{
    public static INodeRunner Wrap(
        INodeRunner inner,
        SourceFailurePolicy policy,
        Action<string, string>? log,
        Func<CancellationToken, Task<INodeRunner>>? rebuild = null) =>
        inner is IRewindableSource
            ? new RewindableResilientSourceRunner(inner, policy, log, rebuild)
            : new ResilientSourceRunner(inner, policy, log, rebuild);
}

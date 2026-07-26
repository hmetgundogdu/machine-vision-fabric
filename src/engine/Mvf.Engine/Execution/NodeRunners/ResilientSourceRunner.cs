using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps a source runner so a read failure (a camera timing out, a dropped connection) is retried per a
/// <see cref="SourceFailurePolicy"/> instead of ending the whole run. The executors call
/// <c>ExecuteAsync</c> exactly as before — the retrying happens inside here, so neither executor changes.
///
/// <para>Semantics:</para>
/// <list type="bullet">
///   <item>A clean <see cref="NodeExecutionResult.NoOutput"/> (end of stream) is passed straight through —
///     an exhausted source is not a failure and is never retried.</item>
///   <item>An exception reconnects the source (re-opens its stream via <see cref="INodeRunner.ActivateAsync"/>)
///     after a backoff and reads again. <c>ExecuteAsync</c> only runs against a freshly reconnected runner,
///     so a returned NoOutput always means a genuine end of stream, never a stale reader.</item>
///   <item>When the policy's attempts are exhausted (bounded <see cref="SourceFailureMode.Retry"/>) the last
///     error is rethrown, so the executor fails the run exactly as it would have without this wrapper — the
///     honesty guarantee is preserved. <see cref="SourceFailureMode.Reconnect"/> retries until cancelled.</item>
/// </list>
///
/// <para>Only the read path is made resilient; the initial activation at pipeline start stays fail-fast, so a
/// source that is entirely absent when the run begins still reports that immediately.</para>
/// </summary>
internal class ResilientSourceRunner(
    INodeRunner inner,
    SourceFailurePolicy policy,
    Action<string, string>? log = null) : INodeRunner
{
    /// <summary>The wrapped runner. Exposed to the rewindable subclass.</summary>
    protected INodeRunner Inner { get; } = inner;

    public string NodeId => Inner.NodeId;

    // Initial activation is deliberately not retried — a source that never comes up at start is reported now.
    public Task ActivateAsync(CancellationToken cancellationToken) => Inner.ActivateAsync(cancellationToken);

    public ValueTask DisposeAsync() => Inner.DisposeAsync();

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        // One shared attempt budget for the whole frame-pull: every read failure and every failed reconnect
        // counts against it, so a bounded Retry can never loop forever. Reset per pull, so each frame gets a
        // fresh budget rather than the run draining one over its lifetime.
        var attempt = 0;

        while (true)
        {
            try
            {
                return await Inner.ExecuteAsync(inputs, cancellationToken);
            }
            catch (Exception readError) when (readError is not OperationCanceledException)
            {
                // Reconnect (sharing the budget) until one succeeds, then loop to read again. We only ever
                // read against a freshly reconnected runner, so a subsequent NoOutput is a genuine end of
                // stream — never a faulted reader reporting empty.
                var lastError = readError;
                while (true)
                {
                    attempt++;
                    if (!policy.AllowsAttempt(attempt))
                    {
                        log?.Invoke("error", $"source failed after {attempt - 1} reconnect attempt(s): {lastError.Message}");
                        throw lastError;
                    }

                    var backoff = policy.BackoffFor(attempt);
                    var bound   = policy.Mode == SourceFailureMode.Retry ? $"/{policy.MaxRetries}" : string.Empty;
                    log?.Invoke("warn", $"source error (attempt {attempt}{bound}), reconnecting in {backoff.TotalMilliseconds:F0}ms: {lastError.Message}");

                    await Task.Delay(backoff, cancellationToken);

                    try
                    {
                        await Inner.ActivateAsync(cancellationToken);
                        break; // reconnected — read again
                    }
                    catch (Exception reconnectError) when (reconnectError is not OperationCanceledException)
                    {
                        lastError = reconnectError; // count it and keep trying (or exhaust)
                    }
                }
            }
        }
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
    Action<string, string>? log = null) : ResilientSourceRunner(inner, policy, log), IRewindableSource
{
    public Task RewindAsync(CancellationToken cancellationToken) =>
        Inner is IRewindableSource rewindable
            ? rewindable.RewindAsync(cancellationToken)
            : Inner.ActivateAsync(cancellationToken);
}

/// <summary>Wraps a source runner for resilience, preserving <see cref="IRewindableSource"/> when the inner runner has it.</summary>
internal static class ResilientSourceRunnerFactory
{
    public static INodeRunner Wrap(INodeRunner inner, SourceFailurePolicy policy, Action<string, string>? log) =>
        inner is IRewindableSource
            ? new RewindableResilientSourceRunner(inner, policy, log)
            : new ResilientSourceRunner(inner, policy, log);
}

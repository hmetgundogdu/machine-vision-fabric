using System.Text.Json.Nodes;

namespace Mvf.Graph.Execution;

/// <summary>What a run does when a node throws while executing (a camera timing out, a sink losing its connection).</summary>
public enum NodeFailureMode
{
    /// <summary>
    /// Let the failure fall through to the executor's default for the node's role: a <b>source</b> ends the
    /// run (a faulted source is not a clean, empty success), while a mid-graph node's cycle is skipped and the
    /// run carries on. This is the honest baseline and the default for every node.
    /// </summary>
    Fail,

    /// <summary>
    /// Restart the node — dispose it and bring it back from scratch (a fresh session/model) — then carry on.
    /// <see cref="NodeFailurePolicy.Limit"/> caps how many restarts before the failure falls through to the
    /// role default above; 0 means forever. One general action (it fits a camera, a file, a classifier, a
    /// sink alike); "how persistent" is the number, not a second mode.
    /// </summary>
    Restart
}

/// <summary>
/// How the runtime handles a node that fails while executing.
///
/// <para>The failure policy is a <b>pipeline/runtime</b> concern, not a module one: a module's job is to throw
/// and say what broke, but whether that ends the run, skips a cycle, or restarts the node is an operational
/// choice that differs per deployment. So it lives here, settable per node (the node's <c>onError</c> config)
/// with a source-level run default, and applied uniformly by a decorator around the node runner — the
/// executors are unaware of it.</para>
///
/// <para>There is deliberately one recovery action, <see cref="NodeFailureMode.Restart"/>, with a numeric
/// <see cref="Limit"/> rather than separate "retry"/"reconnect" modes: bounded vs. forever is a count, not a
/// different verb. The default is <see cref="NodeFailureMode.Fail"/> so existing behaviour holds unless a
/// deployment opts into recovery — a faulted source still ends the run, a faulted mid-graph node still skips.</para>
/// </summary>
public sealed record NodeFailurePolicy
{
    /// <summary>What to do when the node throws.</summary>
    public NodeFailureMode Mode { get; init; } = NodeFailureMode.Fail;

    /// <summary>How many restarts before the failure falls through to the role default. <c>0</c> (or negative) means restart forever.</summary>
    public int Limit { get; init; }

    /// <summary>First backoff, doubled each attempt up to <see cref="MaxBackoffMs"/>.</summary>
    public int BaseBackoffMs { get; init; } = 500;

    /// <summary>Upper bound on a single backoff wait.</summary>
    public int MaxBackoffMs { get; init; } = 5_000;

    /// <summary>The honest fall-through default (source ends the run, mid-graph node skips).</summary>
    public static NodeFailurePolicy Fail { get; } = new();

    /// <summary>True when the policy restarts at all (i.e. the node runner needs wrapping).</summary>
    public bool WillRestart => Mode == NodeFailureMode.Restart;

    /// <summary>Whether restart attempt <paramref name="attempt"/> (1-based) is allowed. Unbounded when <see cref="Limit"/> ≤ 0.</summary>
    public bool AllowsAttempt(int attempt) =>
        Mode == NodeFailureMode.Restart && (Limit <= 0 || attempt <= Limit);

    /// <summary>The backoff before restart attempt <paramref name="attempt"/> (1-based): exponential, capped.</summary>
    public TimeSpan BackoffFor(int attempt)
    {
        var shift = Math.Clamp(attempt - 1, 0, 30);
        var ms    = Math.Min((long)Math.Max(0, BaseBackoffMs) << shift, Math.Max(0, MaxBackoffMs));
        return TimeSpan.FromMilliseconds(ms);
    }

    /// <summary>
    /// Reads a node's <c>onError</c> config, falling back to <paramref name="fallback"/>. Accepts a string
    /// shorthand (<c>"fail"</c>/<c>"restart"</c>) or an object <c>{ mode, limit, backoffMs, maxBackoffMs }</c>.
    /// An unrecognised value keeps the fallback.
    /// </summary>
    public static NodeFailurePolicy FromConfig(JsonObject? config, NodeFailurePolicy fallback)
    {
        if (config is null || !config.TryGetPropertyValue("onError", out var node) || node is null)
        {
            return fallback;
        }

        if (node is JsonValue s && s.TryGetValue(out string? text) && text is not null)
        {
            return TryParseMode(text, out var mode) ? fallback with { Mode = mode } : fallback;
        }

        if (node is JsonObject obj)
        {
            var mode = obj.TryGetPropertyValue("mode", out var mn)
                       && mn is JsonValue mv && mv.TryGetValue(out string? ms) && TryParseMode(ms, out var pm)
                ? pm
                : fallback.Mode;

            var policy = fallback with { Mode = mode };
            // 'limit' is canonical; 'maxRetries' is accepted as an alias for the earlier field name.
            if (TryReadInt(obj, "limit",         out var lim)) policy = policy with { Limit = lim };
            else if (TryReadInt(obj, "maxRetries", out var mr)) policy = policy with { Limit = mr };
            if (TryReadInt(obj, "backoffMs",     out var bo)) policy = policy with { BaseBackoffMs = bo };
            if (TryReadInt(obj, "maxBackoffMs",  out var mb)) policy = policy with { MaxBackoffMs  = mb };
            return policy;
        }

        return fallback;
    }

    /// <summary>
    /// Parses a mode token. Canonical: <c>"fail"</c> / <c>"restart"</c>. The earlier <c>"retry"</c>,
    /// <c>"reconnect"</c> and <c>"forever"</c> tokens are accepted as aliases for <c>restart</c> so older
    /// configs and commands keep working.
    /// </summary>
    public static bool TryParseMode(string? text, out NodeFailureMode mode)
    {
        switch (text?.Trim().ToLowerInvariant())
        {
            case "fail":
                mode = NodeFailureMode.Fail;
                return true;
            case "restart":
            case "retry":
            case "reconnect":
            case "forever":
                mode = NodeFailureMode.Restart;
                return true;
            default:
                mode = NodeFailureMode.Fail;
                return false;
        }
    }

    private static bool TryReadInt(JsonObject obj, string key, out int value)
    {
        value = 0;
        return obj.TryGetPropertyValue(key, out var n) && n is JsonValue jv && jv.TryGetValue(out value);
    }
}

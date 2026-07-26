using System.Text.Json.Nodes;

namespace Mvf.Graph.Execution;

/// <summary>What a run does when a <b>source</b> node throws mid-stream (a camera timing out, a stream dropping).</summary>
public enum SourceFailureMode
{
    /// <summary>End the run with the source's error. The honest default: a faulted source is not a clean, empty success.</summary>
    Fail,

    /// <summary>
    /// Restart the node — dispose it and bring it back from scratch (a fresh session) — then carry on.
    /// <see cref="SourceFailurePolicy.Limit"/> caps how many restarts before the run gives up; 0 means forever.
    /// This is one general action (it fits a camera, a file, a simulator alike); "how persistent" is the number,
    /// not a second mode.
    /// </summary>
    Restart
}

/// <summary>
/// How the runtime handles a source node that fails while producing frames.
///
/// <para>The failure policy is a <b>pipeline/runtime</b> concern, not a module one: a module's job is to throw
/// and say what broke, but whether that ends the whole run is an operational choice that differs per
/// deployment (a CI check wants <see cref="SourceFailureMode.Fail"/>; a panel PC on a line wants an unbounded
/// <see cref="SourceFailureMode.Restart"/>). So it lives here, settable per source (the node's <c>onError</c>
/// config) with a run-level default, and applied uniformly by a decorator around the source runner — the
/// executors are unaware of it.</para>
///
/// <para>There is deliberately one recovery action, <see cref="SourceFailureMode.Restart"/>, with a numeric
/// <see cref="Limit"/> rather than separate "retry"/"reconnect" modes: bounded vs. forever is a count, not a
/// different verb. The default is <see cref="SourceFailureMode.Fail"/> so the "a faulted source is not a clean
/// success" guarantee holds unless a deployment opts into recovery.</para>
/// </summary>
public sealed record SourceFailurePolicy
{
    /// <summary>What to do on a source read failure.</summary>
    public SourceFailureMode Mode { get; init; } = SourceFailureMode.Fail;

    /// <summary>How many restarts before the run fails. <c>0</c> (or negative) means restart forever.</summary>
    public int Limit { get; init; }

    /// <summary>First backoff, doubled each attempt up to <see cref="MaxBackoffMs"/>.</summary>
    public int BaseBackoffMs { get; init; } = 500;

    /// <summary>Upper bound on a single backoff wait.</summary>
    public int MaxBackoffMs { get; init; } = 5_000;

    /// <summary>The honest fail-fast default.</summary>
    public static SourceFailurePolicy Fail { get; } = new();

    /// <summary>True when the policy restarts at all (i.e. the source runner needs wrapping).</summary>
    public bool WillRestart => Mode == SourceFailureMode.Restart;

    /// <summary>Whether restart attempt <paramref name="attempt"/> (1-based) is allowed. Unbounded when <see cref="Limit"/> ≤ 0.</summary>
    public bool AllowsAttempt(int attempt) =>
        Mode == SourceFailureMode.Restart && (Limit <= 0 || attempt <= Limit);

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
    public static SourceFailurePolicy FromConfig(JsonObject? config, SourceFailurePolicy fallback)
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
            if (TryReadInt(obj, "limit",        out var lim)) policy = policy with { Limit = lim };
            else if (TryReadInt(obj, "maxRetries", out var mr)) policy = policy with { Limit = mr };
            if (TryReadInt(obj, "backoffMs",    out var bo)) policy = policy with { BaseBackoffMs = bo };
            if (TryReadInt(obj, "maxBackoffMs", out var mb)) policy = policy with { MaxBackoffMs  = mb };
            return policy;
        }

        return fallback;
    }

    /// <summary>
    /// Parses a mode token. Canonical: <c>"fail"</c> / <c>"restart"</c>. The earlier <c>"retry"</c>,
    /// <c>"reconnect"</c> and <c>"forever"</c> tokens are accepted as aliases for <c>restart</c> so older
    /// configs and commands keep working.
    /// </summary>
    public static bool TryParseMode(string? text, out SourceFailureMode mode)
    {
        switch (text?.Trim().ToLowerInvariant())
        {
            case "fail":
                mode = SourceFailureMode.Fail;
                return true;
            case "restart":
            case "retry":
            case "reconnect":
            case "forever":
                mode = SourceFailureMode.Restart;
                return true;
            default:
                mode = SourceFailureMode.Fail;
                return false;
        }
    }

    private static bool TryReadInt(JsonObject obj, string key, out int value)
    {
        value = 0;
        return obj.TryGetPropertyValue(key, out var n) && n is JsonValue jv && jv.TryGetValue(out value);
    }
}

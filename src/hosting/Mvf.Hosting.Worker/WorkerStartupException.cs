namespace Mvf.Hosting.Worker;

/// <summary>
/// A worker failed to <b>start up</b>: it did not hand-shake or did not signal readiness within the
/// startup budget (a slow model load / device connect that overran, or a hung child). Distinct from a
/// mid-run crash (liveness) so a slow warmup is never mistaken for a hang — the Kubernetes
/// startup-vs-liveness separation. See <c>docs/module-lifecycle-design.md</c>.
/// </summary>
public sealed class WorkerStartupException(string message) : Exception(message);

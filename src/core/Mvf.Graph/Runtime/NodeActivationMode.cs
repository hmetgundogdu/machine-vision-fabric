namespace Mvf.Graph.Runtime;

/// <summary>
/// A node's <b>loading profile</b> — when the engine activates it and how long it stays warm. Part of the
/// node contract (CLAUDE.md), not an implementation detail. Generalizes past ML models: "activation" may
/// be loading a model, loading a package, connecting a camera/PLC, or a module finishing its own init.
/// Aligned to the Triton model-control taxonomy (see <c>docs/module-lifecycle-design.md</c>).
/// </summary>
public enum NodeActivationMode
{
    /// <summary>Load at startup, keep warm for the whole run (preloaded). Default for models, cameras, PLCs.</summary>
    Resident = 0,

    /// <summary>Activate on first use; a short helper the engine need not keep resident. (Lazy behavior lands in L.3.)</summary>
    OnDemand = 1
}

/// <summary>Parsing for the <c>activationMode</c> / manifest <c>lifecycle</c> string, so the field is a real,
/// validated contract instead of a decorative string.</summary>
public static class NodeActivationModes
{
    /// <summary>The accepted string values, for validation messages.</summary>
    public const string Supported = "resident, on-demand";

    /// <summary>Parses "resident" / "on-demand" (case-insensitive; "ondemand" also accepted). Unknown → false.</summary>
    public static bool TryParse(string? value, out NodeActivationMode mode)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "resident":
                mode = NodeActivationMode.Resident;
                return true;
            case "on-demand":
            case "ondemand":
                mode = NodeActivationMode.OnDemand;
                return true;
            default:
                mode = NodeActivationMode.Resident;
                return false;
        }
    }

    public static string ToWireString(NodeActivationMode mode) =>
        mode == NodeActivationMode.OnDemand ? "on-demand" : "resident";
}

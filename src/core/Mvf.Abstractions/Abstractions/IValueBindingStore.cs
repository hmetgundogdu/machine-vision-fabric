using System.Text.Json.Nodes;

namespace Mvf.Abstractions;

/// <summary>
/// Machine-local storage for resolved values, keyed by binding name.
///
/// <para><b>Outside the package, by design.</b> The same <c>pipeline.json</c> deploys to ten panel PCs
/// and each binds to its own camera; writing the choice back into the pipeline file would turn a
/// versioned, portable artifact into a machine-specific one.</para>
/// </summary>
public interface IValueBindingStore
{
    Task<IReadOnlyDictionary<string, JsonNode?>> LoadAsync(CancellationToken cancellationToken);

    Task SaveAsync(IReadOnlyDictionary<string, JsonNode?> bindings, CancellationToken cancellationToken);

    /// <summary>Where the bindings live, for error messages that tell an operator what to edit.</summary>
    string Location { get; }
}

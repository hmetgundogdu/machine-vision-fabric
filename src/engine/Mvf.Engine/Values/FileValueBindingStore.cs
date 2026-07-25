using System.Text.Json;
using System.Text.Json.Nodes;
using Mvf.Abstractions;

namespace Mvf.Engine.Values;

/// <summary>
/// Bindings on disk as a flat JSON object of name → value, at <c>.mvf/bindings.json</c> next to the
/// checkpoint directory.
///
/// <para>A missing or unreadable file is an empty store, not an error: the first run on a machine has
/// no bindings yet, and that is the normal case rather than a failure.</para>
/// </summary>
public sealed class FileValueBindingStore(string path) : IValueBindingStore
{
    private static readonly JsonSerializerOptions WriteOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public string Location => path;

    public async Task<IReadOnlyDictionary<string, JsonNode?>> LoadAsync(CancellationToken cancellationToken)
    {
        var bindings = new Dictionary<string, JsonNode?>(StringComparer.Ordinal);

        if (!File.Exists(path))
        {
            return bindings;
        }

        JsonObject? root;
        try
        {
            var json = await File.ReadAllTextAsync(path, cancellationToken);
            root = JsonNode.Parse(json) as JsonObject;
        }
        catch (Exception ex) when (ex is IOException or JsonException or UnauthorizedAccessException)
        {
            return bindings;
        }

        if (root is null)
        {
            return bindings;
        }

        foreach (var (name, value) in root)
        {
            bindings[name] = value?.DeepClone();
        }

        return bindings;
    }

    public async Task SaveAsync(IReadOnlyDictionary<string, JsonNode?> bindings, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(bindings);

        var root = new JsonObject();
        foreach (var (name, value) in bindings.OrderBy(b => b.Key, StringComparer.Ordinal))
        {
            root[name] = value?.DeepClone();
        }

        var directory = Path.GetDirectoryName(Path.GetFullPath(path));
        if (directory is { Length: > 0 })
        {
            Directory.CreateDirectory(directory);
        }

        await File.WriteAllTextAsync(path, root.ToJsonString(WriteOptions), cancellationToken);
    }
}

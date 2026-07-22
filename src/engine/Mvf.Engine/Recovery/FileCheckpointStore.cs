using Mvf.Abstractions;

namespace Mvf.Engine.Recovery;

/// <summary>
/// A file-backed <see cref="ICheckpointStore"/>: each node's state is a <c>&lt;nodeId&gt;.state</c> file of
/// raw bytes in the checkpoint directory (no base64). Each file is written via a temp file + atomic
/// rename, so a crash mid-save never leaves a torn state file. Being on disk, the capture survives a
/// whole-process crash; the file-backed data arena has the same property.
/// </summary>
public sealed class FileCheckpointStore(string directory) : ICheckpointStore
{
    private const string StateExtension = ".state";

    public async Task SaveAsync(IReadOnlyDictionary<string, byte[]> statesByNodeId, CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(directory);
        foreach (var (nodeId, state) in statesByNodeId)
        {
            var path = StatePath(nodeId);
            if (path is null)
            {
                continue; // skip an unsafe node id rather than write outside the directory
            }

            var temp = path + ".tmp";
            await File.WriteAllBytesAsync(temp, state, cancellationToken);
            File.Move(temp, path, overwrite: true);
        }
    }

    public async Task<IReadOnlyDictionary<string, byte[]>> LoadAsync(CancellationToken cancellationToken)
    {
        var result = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        if (!Directory.Exists(directory))
        {
            return result;
        }

        foreach (var file in Directory.EnumerateFiles(directory, "*" + StateExtension))
        {
            var nodeId = Path.GetFileNameWithoutExtension(file);
            result[nodeId] = await File.ReadAllBytesAsync(file, cancellationToken);
        }

        return result;
    }

    public Task ClearAsync(CancellationToken cancellationToken)
    {
        if (Directory.Exists(directory))
        {
            Directory.Delete(directory, recursive: true);
        }

        return Task.CompletedTask;
    }

    private string? StatePath(string nodeId)
    {
        var fileName = nodeId + StateExtension;
        // Reject a node id that would escape the directory (path separators / traversal).
        if (fileName.Contains(Path.DirectorySeparatorChar) || fileName.Contains(Path.AltDirectorySeparatorChar)
            || !string.Equals(Path.GetFileName(fileName), fileName, StringComparison.Ordinal))
        {
            return null;
        }

        return Path.Combine(directory, fileName);
    }
}

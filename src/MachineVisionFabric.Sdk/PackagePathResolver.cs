namespace MachineVisionFabric.Sdk;

public static class PackagePathResolver
{
    public static string Resolve(string packageRoot, string path)
    {
        return Path.IsPathRooted(path)
            ? Path.GetFullPath(path)
            : Path.GetFullPath(Path.Combine(packageRoot, path));
    }
}

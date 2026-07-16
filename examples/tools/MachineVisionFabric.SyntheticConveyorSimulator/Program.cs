using System.Text.Json;

var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web)
{
    WriteIndented = true
};

var options = SimulatorOptions.Parse(args);
if (options.ShowHelp)
{
    PrintHelp();
    return;
}

var outputRoot = Path.GetFullPath(options.OutputRoot);
var imagesRoot = Path.Combine(outputRoot, "images");
var annotationsRoot = Path.Combine(outputRoot, "annotations");

if (options.Clean && Directory.Exists(outputRoot))
{
    Directory.Delete(outputRoot, recursive: true);
}

Directory.CreateDirectory(imagesRoot);
Directory.CreateDirectory(annotationsRoot);

var summary = new SimulationSummary
{
    Name = options.Name,
    Width = options.Width,
    Height = options.Height,
    FrameCount = options.FrameCount,
    Frames = new List<SimulationFrameAnnotation>(options.FrameCount)
};

for (var i = 0; i < options.FrameCount; i++)
{
    var frame = BuildFrame(options, i);
    var imagePath = Path.Combine(imagesRoot, frame.FileName);
    WriteBmp(imagePath, options.Width, options.Height, frame.Pixels);

    var annotationPath = Path.Combine(annotationsRoot, Path.GetFileNameWithoutExtension(frame.FileName) + ".json");
    File.WriteAllText(annotationPath, JsonSerializer.Serialize(frame.Annotation, jsonOptions));
    summary.Frames.Add(frame.Annotation);
}

File.WriteAllText(Path.Combine(outputRoot, "simulation.json"), JsonSerializer.Serialize(summary, jsonOptions));

Console.WriteLine($"OutputRoot: {outputRoot}");
Console.WriteLine($"ImagesRoot: {imagesRoot}");
Console.WriteLine($"AnnotationsRoot: {annotationsRoot}");
Console.WriteLine($"FrameCount: {options.FrameCount}");

return;

SimulationFrame BuildFrame(SimulatorOptions options, int frameIndex)
{
    var pixels = new byte[options.Width * options.Height * 3];

    Fill(pixels, options.Width, options.Height, 235, 239, 243);
    FillRect(pixels, options.Width, options.Height, 0, options.Height / 3, options.Width, options.Height / 3, 58, 62, 68);
    FillRect(pixels, options.Width, options.Height, 0, options.Height / 3 - 8, options.Width, 8, 90, 96, 104);
    FillRect(pixels, options.Width, options.Height, 0, options.Height * 2 / 3, options.Width, 8, 90, 96, 104);
    FillRect(pixels, options.Width, options.Height, options.Width / 2 - 6, options.Height / 3 - 22, 12, options.Height / 3 + 44, 120, 180, 220);

    for (var stripeX = -40; stripeX < options.Width + 40; stripeX += 70)
    {
        var animatedX = stripeX + (frameIndex * 18 % 70);
        FillRect(pixels, options.Width, options.Height, animatedX, options.Height / 2 - 4, 30, 8, 80, 84, 92);
    }

    var productPresent = frameIndex >= options.EmptyLeadFrames;
    BoundingBox? bbox = null;
    if (productPresent)
    {
        var travelFrame = frameIndex - options.EmptyLeadFrames;
        var productWidth = 96;
        var productHeight = 60;
        var startX = -productWidth;
        var endX = options.Width + 24;
        var progress = options.FrameCount - options.EmptyLeadFrames <= 1
            ? 1d
            : travelFrame / (double)(options.FrameCount - options.EmptyLeadFrames - 1);
        var productX = (int)Math.Round(startX + (endX - startX) * progress);
        var productY = options.Height / 2 - productHeight / 2;

        FillRect(pixels, options.Width, options.Height, productX, productY, productWidth, productHeight, 237, 146, 55);
        FillRect(pixels, options.Width, options.Height, productX + 8, productY + 8, productWidth - 16, productHeight - 16, 246, 196, 95);
        FillRect(pixels, options.Width, options.Height, productX + 20, productY + 18, 18, 18, 48, 62, 78);
        FillRect(pixels, options.Width, options.Height, productX + productWidth - 26, productY + 18, 10, 24, 191, 57, 43);

        bbox = new BoundingBox
        {
            X = productX,
            Y = productY,
            Width = productWidth,
            Height = productHeight
        };
    }

    var sensorActive = productPresent && bbox is not null && bbox.X <= options.Width / 2 && bbox.X + bbox.Width >= options.Width / 2;
    var sensorColor = sensorActive
        ? (R: (byte)54, G: (byte)179, B: (byte)97)
        : (R: (byte)191, G: (byte)57, B: (byte)43);
    FillRect(pixels, options.Width, options.Height, options.Width / 2 - 10, options.Height / 3 - 40, 20, 14, sensorColor.R, sensorColor.G, sensorColor.B);

    var fileName = $"frame-{frameIndex + 1:0000}.bmp";
    var annotation = new SimulationFrameAnnotation
    {
        FileName = fileName,
        SequenceNumber = frameIndex + 1,
        ProductPresent = productPresent,
        SensorActive = sensorActive,
        BoundingBox = bbox
    };

    return new SimulationFrame(fileName, pixels, annotation);
}

void Fill(byte[] pixels, int width, int height, byte r, byte g, byte b)
{
    for (var y = 0; y < height; y++)
    {
        for (var x = 0; x < width; x++)
        {
            SetPixel(pixels, width, height, x, y, r, g, b);
        }
    }
}

void FillRect(byte[] pixels, int width, int height, int x, int y, int rectWidth, int rectHeight, byte r, byte g, byte b)
{
    var startX = Math.Max(0, x);
    var startY = Math.Max(0, y);
    var endX = Math.Min(width, x + rectWidth);
    var endY = Math.Min(height, y + rectHeight);

    for (var yy = startY; yy < endY; yy++)
    {
        for (var xx = startX; xx < endX; xx++)
        {
            SetPixel(pixels, width, height, xx, yy, r, g, b);
        }
    }
}

void SetPixel(byte[] pixels, int width, int height, int x, int y, byte r, byte g, byte b)
{
    if (x < 0 || y < 0 || x >= width || y >= height)
    {
        return;
    }

    var row = height - 1 - y;
    var index = (row * width + x) * 3;
    pixels[index] = b;
    pixels[index + 1] = g;
    pixels[index + 2] = r;
}

void WriteBmp(string path, int width, int height, byte[] pixels)
{
    var rowPadding = (4 - (width * 3) % 4) % 4;
    var stride = width * 3 + rowPadding;
    var pixelDataSize = stride * height;
    var fileSize = 54 + pixelDataSize;

    using var stream = File.Create(path);
    using var writer = new BinaryWriter(stream);

    writer.Write((byte)'B');
    writer.Write((byte)'M');
    writer.Write(fileSize);
    writer.Write((short)0);
    writer.Write((short)0);
    writer.Write(54);

    writer.Write(40);
    writer.Write(width);
    writer.Write(height);
    writer.Write((short)1);
    writer.Write((short)24);
    writer.Write(0);
    writer.Write(pixelDataSize);
    writer.Write(2835);
    writer.Write(2835);
    writer.Write(0);
    writer.Write(0);

    var rowBuffer = new byte[stride];
    for (var row = 0; row < height; row++)
    {
        Buffer.BlockCopy(pixels, row * width * 3, rowBuffer, 0, width * 3);
        Array.Clear(rowBuffer, width * 3, rowPadding);
        writer.Write(rowBuffer);
    }
}

void PrintHelp()
{
    Console.WriteLine("MachineVisionFabric.SyntheticConveyorSimulator");
    Console.WriteLine("Options:");
    Console.WriteLine("  --output <path>       Output root. Default: .\\artifacts\\synthetic-conveyor");
    Console.WriteLine("  --name <value>        Scenario name. Default: conveyor-basic");
    Console.WriteLine("  --frames <number>     Frame count. Default: 16");
    Console.WriteLine("  --width <number>      Frame width. Default: 640");
    Console.WriteLine("  --height <number>     Frame height. Default: 360");
    Console.WriteLine("  --empty-lead <num>    Empty frames before product appears. Default: 3");
    Console.WriteLine("  --clean               Delete output directory before generation.");
}

internal sealed record SimulationFrame(string FileName, byte[] Pixels, SimulationFrameAnnotation Annotation);

internal sealed class SimulationSummary
{
    public required string Name { get; init; }
    public required int Width { get; init; }
    public required int Height { get; init; }
    public required int FrameCount { get; init; }
    public required List<SimulationFrameAnnotation> Frames { get; init; }
}

internal sealed class SimulationFrameAnnotation
{
    public required string FileName { get; init; }
    public required int SequenceNumber { get; init; }
    public required bool ProductPresent { get; init; }
    public required bool SensorActive { get; init; }
    public BoundingBox? BoundingBox { get; init; }
}

internal sealed class BoundingBox
{
    public required int X { get; init; }
    public required int Y { get; init; }
    public required int Width { get; init; }
    public required int Height { get; init; }
}

internal sealed class SimulatorOptions
{
    public required bool ShowHelp { get; init; }
    public required bool Clean { get; init; }
    public required string OutputRoot { get; init; }
    public required string Name { get; init; }
    public required int FrameCount { get; init; }
    public required int Width { get; init; }
    public required int Height { get; init; }
    public required int EmptyLeadFrames { get; init; }

    public static SimulatorOptions Parse(string[] args)
    {
        var options = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        for (var i = 0; i < args.Length; i++)
        {
            var token = args[i];
            if (!token.StartsWith("--", StringComparison.Ordinal))
            {
                continue;
            }

            var key = token[2..];
            var value = i + 1 < args.Length && !args[i + 1].StartsWith("--", StringComparison.Ordinal)
                ? args[++i]
                : "true";
            options[key] = value;
        }

        return new SimulatorOptions
        {
            ShowHelp = options.ContainsKey("help") || options.ContainsKey("h"),
            Clean = options.ContainsKey("clean"),
            OutputRoot = options.TryGetValue("output", out var outputRoot) ? outputRoot : ".\\artifacts\\synthetic-conveyor",
            Name = options.TryGetValue("name", out var name) ? name : "conveyor-basic",
            FrameCount = TryReadInt(options, "frames", 16),
            Width = TryReadInt(options, "width", 640),
            Height = TryReadInt(options, "height", 360),
            EmptyLeadFrames = TryReadInt(options, "empty-lead", 3)
        };
    }

    private static int TryReadInt(IReadOnlyDictionary<string, string> options, string key, int defaultValue)
    {
        return options.TryGetValue(key, out var raw) && int.TryParse(raw, out var value)
            ? value
            : defaultValue;
    }
}

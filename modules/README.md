# MachineVisionFabric — Real-World Projects

Bu dizin gerçek donanımlara bağlanan entegrasyon modüllerini ve hazır senaryoları içerir.

---

## Senaryo: Cognex In-Sight — Dark Frame Filter — Dataset Writer

**Pipeline:**
```
Cognex Camera (source)
    → frame →
Dark Frame Filter (compute)       ← karanlık frame'leri atar
    → frame (sadece parlak frame) →
Dataset Writer (output)
```

Sürekli çalışır, Ctrl+C ile durdurulur.

---

## Kamera IP'sini Nasıl Değiştirirsin?

`packages/cognex-dark-filter-dataset/pipeline.json` dosyasını aç:

```json
{
  "id": "camera1",
  "config": {
    "ipAddress": "192.168.1.11",   ← BUNU DEĞİŞTİR
    "cameraId": "cognex-cam-1",
    ...
  }
}
```

---

## Publish ve Çalıştırma

```powershell
# Repo kökünden
powershell -File publish.ps1 -IncludeRealWorld

# Çalıştır
cd publish\mvf
.\MachineVisionFabric.Cli.exe execute-graph --package packages\cognex-dark-filter-dataset
```

---

## Karanlık Eşik Ayarı

`pipeline.json` → `dark-filter` node:

```json
"config": {
  "minimumMeanBrightness": 18.0   ← 0–255 arası, düşür = daha az filtre, artır = daha katı
}
```

---

## Entegrasyon Modülleri

| Modül | ID | Açıklama |
|---|---|---|
| CognexCamera | `mvf.realworld-cognex-camera` | Cognex In-Sight HMI WebSocket kaynak |
| DarkFrameFilter | `mvf.realworld-dark-frame-filter` | OpenCV ortalama parlaklık filtresi |

`mvf.dataset-writer` platform `examples/integrations/` içindedir.

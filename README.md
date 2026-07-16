# MachineVisionFabric

MachineVisionFabric, endüstriyel ve genel amaçlı görüntü kaynaklarını ortak bir entegrasyon modeliyle alıp diyagram tabanlı veri işleme akışlarında çalıştırmayı hedefleyen açık kaynak bir pipeline platformudur.

İlk odak:

- Kamera ve image stream kaynaklarını ortak adapter sözleşmesiyle sisteme bağlamak
- Dinamik trigger, config ve event kuralları tanımlamak
- Node/edge tabanlı gelişmiş bir diagram sistemi ile pipeline kurmak
- Python, exe, AI model ve yerel işlem adımlarını aynı akışta sıralı veya dallanan biçimde çalıştırmak
- Ham görüntüyü ve pipeline çıktılarını ağ üzerinden yayınlayabilmek

Dağıtım varsayımı:

- Sistem şirket ağı içinde çalışacak
- Çalışma hedefleri panel PC ve Intel NUC benzeri edge cihazlar
- İnternet bağımsız veya kısıtlı ağ koşulları desteklenmeli
- Merkezi sunucuya bağımlı olmadan yerelde çalışabilmeli

## Başlangıç Mimari Yönü

Bu repo için ilk öneri yön:

- Runtime çekirdeği: `.NET` tabanlı orchestration engine
- Görsel akış editörü: web tabanlı node editor
- AI ve özel adımlar: `.NET`, Python ve dış process node desteği
- Streaming katmanı: standart protokolleri kullanan ayrı media gateway
- Dağıtık event omurgası: opsiyonel message bus

Bu yön seçildi çünkü eski projede Windows donanım entegrasyonları ve eşzamanlı işleme tarafı zaten güçlü; yeni projede eksik olan taraf sabit pipeline yerine dinamik graph, plugin sözleşmesi ve standart ağ protokolleri.

## Dokümanlar

- [Eski Proje Bulguları](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\legacy-project-findings.md)
- [Mimari Temel](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\architecture-foundation.md)
- [Teknoloji Önerisi](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\technology-recommendation.md)
- [Açık Sorular](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\open-questions.md)
- [Session Handoff 2026-07-16](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\session-handoff-2026-07-16.md)
- [AGENTS.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\AGENTS.md)
- [CLAUDE.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\CLAUDE.md)

## İlk Repo İskeleti

```text
MachineVisionFabric/
├─ docs/
├─ src/
├─ tools/
└─ samples/
```

Şimdilik bu repo analiz ve yön belirleme aşamasında. Uygulama iskeletini bir sonraki adımda seçilecek runtime ve UI kararlarına göre oluşturacağız.

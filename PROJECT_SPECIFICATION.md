# CAFE Format - Especificação Técnica Detalhada

## Compression Adaptive Filtering Experiment

**Versão**: 1.0 (Especificação Completa)
**Data**: Fevereiro 2026
**Autor**: Daniel Secco
**Status**: Documento de Especificação para Implementação Completa

---

## 1. Visão Geral do Projeto

### 1.1 Objetivo

O CAFE (Compression Adaptive Filtering Experiment) é um formato de imagem experimental projetado especificamente para pipelines de machine learning e pesquisa científica. O formato visa resolver os gargalos de I/O que limitam o desempenho de tarefas de visão computacional em larga escala.

### 1.2 Problema a Resolver

Formatos de imagem tradicionais (PNG, JPEG, etc.) foram projetados para exibição e não para computação intensiva:

- **Overhead do sistema de arquivos**: Milhares de arquivos pequenos causam latência significativa
- **Carregamento serial**: Limitações em throughput de dados
- **Metadados separados**: Anotações, embeddings e labels armazenados separadamente
- **Sem aceleração GPU**: Descompressão limitada à CPU
- **Ineficiência em pipelines**: Tempo desperdiçado em I/O durante treinamento

### 1.3 Proposta de Valor

CAFE oferece:

- **Container único**: Datasets inteiros em um único arquivo otimizado
- **Decodificação paralela**: Blocos independentes processáveis simultaneamente
- **Aceleração GPU**: Descompressão nativa em CUDA
- **Metadados integrados**: IA-ready metadata junto aos pixels
- **Compressão adaptativa**: Seleção automática do melhor codec por bloco

---

## 2. Arquitetura do Formato

### 2.1 Estrutura de Arquivo

```
┌─────────────────────────────────────────────┐
│         CAFE File Structure                  │
├─────────────────────────────────────────────┤
│  [File Header]          (fixo: 256 bytes)   │
├─────────────────────────────────────────────┤
│  [Global Metadata]      (variável)          │
├─────────────────────────────────────────────┤
│  [Image Descriptors]    (array)             │
│    ├─ Image 0 Descriptor                    │
│    ├─ Image 1 Descriptor                    │
│    └─ Image N Descriptor                    │
├─────────────────────────────────────────────┤
│  [Block Index Table]    (offset mapping)    │
├─────────────────────────────────────────────┤
│  [Image Data Blocks]                        │
│    ├─ Image 0                               │
│    │   ├─ Block 0 (128×128)                 │
│    │   ├─ Block 1 (128×128)                 │
│    │   └─ Block N                           │
│    ├─ Image 1                               │
│    │   └─ [blocks...]                       │
│    └─ Image N                               │
├─────────────────────────────────────────────┤
│  [AI Metadata Chunks]   (opcional)          │
│    ├─ Embeddings                            │
│    ├─ Labels                                │
│    ├─ Segmentation Masks                    │
│    └─ Features                              │
├─────────────────────────────────────────────┤
│  [Thumbnail Index]      (opcional)          │
├─────────────────────────────────────────────┤
│  [Footer / Checksum]    (64 bytes)          │
└─────────────────────────────────────────────┘
```

### 2.2 File Header (256 bytes)

```c
typedef struct {
    // Magic Number & Version (16 bytes)
    uint8_t  magic[4];           // "CAFE" (0x43 0x41 0x46 0x45)
    uint16_t version_major;      // Versão principal (1)
    uint16_t version_minor;      // Versão secundária (0)
    uint32_t format_flags;       // Flags de features habilitadas
    uint32_t reserved1;          // Reservado para expansão

    // Container Information (32 bytes)
    uint64_t total_images;       // Número total de imagens
    uint64_t total_blocks;       // Número total de blocos
    uint64_t file_size;          // Tamanho total do arquivo
    uint32_t compression_type;   // Codec padrão usado
    uint32_t reserved2;

    // Offsets (48 bytes)
    uint64_t metadata_offset;    // Offset para Global Metadata
    uint64_t descriptor_offset;  // Offset para Image Descriptors
    uint64_t index_offset;       // Offset para Block Index
    uint64_t data_offset;        // Offset para Image Data
    uint64_t ai_metadata_offset; // Offset para AI Metadata (0 se não usado)
    uint64_t thumbnail_offset;   // Offset para Thumbnails (0 se não usado)

    // Configuration (32 bytes)
    uint16_t block_size;         // Tamanho do bloco (padrão: 128)
    uint8_t  color_depth;        // Bits por canal (8, 10, 12, 16)
    uint8_t  num_channels;       // Canais (1=Gray, 3=RGB, 4=RGBA)
    uint8_t  predictor_type;     // Tipo de preditor usado
    uint8_t  has_alpha;          // 1 se suporta transparência
    uint16_t gpu_optimized;      // 1 se otimizado para GPU
    uint64_t creation_timestamp; // Unix timestamp
    uint64_t last_modified;      // Unix timestamp
    uint32_t creator_id;         // ID da aplicação criadora
    uint32_t reserved3;

    // Checksums & Integrity (48 bytes)
    uint32_t header_crc32;       // CRC-32 do header (até aqui)
    uint8_t  header_sha256[32];  // SHA-256 do header completo
    uint32_t reserved4[3];

    // Reserved for Future Use (80 bytes)
    uint8_t  reserved[80];       // Expansão futura

} cafe_file_header_t;  // Total: 256 bytes
```

### 2.2.1 Pixel Format and HDR Support

CAFE suporta tanto imagens LDR (Low Dynamic Range) tradicionais quanto HDR (High Dynamic Range) para aplicações profissionais e científicas.

```c
// Tipos de pixel suportados
typedef enum {
    CAFE_PIXEL_UINT8 = 0,    // 8-bit unsigned integer (LDR padrão)
    CAFE_PIXEL_UINT10 = 1,   // 10-bit unsigned integer (packed)
    CAFE_PIXEL_UINT12 = 2,   // 12-bit unsigned integer (packed)
    CAFE_PIXEL_UINT16 = 3,   // 16-bit unsigned integer
    CAFE_PIXEL_FLOAT16 = 4,  // 16-bit IEEE 754 half-precision float (HDR)
    CAFE_PIXEL_FLOAT32 = 5,  // 32-bit IEEE 754 single-precision float (HDR)
} cafe_pixel_format_t;

// Espaços de cor suportados
typedef enum {
    CAFE_COLORSPACE_SRGB = 0,      // sRGB (padrão para LDR)
    CAFE_COLORSPACE_LINEAR = 1,    // Linear RGB (padrão para HDR)
    CAFE_COLORSPACE_REC709 = 2,    // Rec. 709 (HDTV)
    CAFE_COLORSPACE_REC2020 = 3,   // Rec. 2020 (UHDTV, HDR)
    CAFE_COLORSPACE_DCIP3 = 4,     // DCI-P3 (Digital Cinema)
    CAFE_COLORSPACE_ACESCG = 5,    // ACEScg (VFX/CGI)
    CAFE_COLORSPACE_ACES2065 = 6,  // ACES 2065-1 (Archival)
    CAFE_COLORSPACE_CUSTOM = 255,  // Custom (metadata descreve)
} cafe_colorspace_t;

// Transfer functions (curvas de gamma)
typedef enum {
    CAFE_TRANSFER_LINEAR = 0,      // Linear (1.0)
    CAFE_TRANSFER_SRGB = 1,        // sRGB (~2.2)
    CAFE_TRANSFER_REC709 = 2,      // Rec. 709
    CAFE_TRANSFER_PQ = 3,          // Perceptual Quantizer (HDR10)
    CAFE_TRANSFER_HLG = 4,         // Hybrid Log-Gamma (HDR)
    CAFE_TRANSFER_GAMMA22 = 5,     // Gamma 2.2
    CAFE_TRANSFER_GAMMA28 = 6,     // Gamma 2.8
} cafe_transfer_function_t;
```

**Características por Formato**:

| Formato       | Tamanho | Range      | Precisão | Uso Típico                      |
|---------------|---------|------------|----------|---------------------------------|
| UINT8         | 1 byte  | [0, 255]   | 256 lvls | LDR padrão (fotos, web)        |
| UINT10        | 1.25 B  | [0, 1023]  | 1024 lvls| Vídeo profissional             |
| UINT12        | 1.5 B   | [0, 4095]  | 4096 lvls| RAW, medical imaging           |
| UINT16        | 2 bytes | [0, 65535] | 65K lvls | Scientific, deep color         |
| FLOAT16 (half)| 2 bytes | [-65K, 65K]| ~3 dígitos| HDR moderado, rendering       |
| FLOAT32       | 4 bytes | [~-10^38]  | ~7 dígitos| HDR científico, VFX           |

**Nota sobre Compressão**:
- Formatos integer (UINT*): Compressão ZSTD muito eficiente (~3:1)
- FLOAT16: Compressão moderada (~2:1), padrão de bits menos regular
- FLOAT32: Compressão limitada (~1.5:1), requer técnicas especializadas

### 2.3 Global Metadata Section

```c
typedef struct {
    uint32_t metadata_size;      // Tamanho total desta seção
    uint32_t num_entries;        // Número de entradas de metadados

    // Array de entradas
    cafe_metadata_entry_t entries[];

} cafe_global_metadata_t;

typedef struct {
    uint32_t key_length;         // Tamanho da chave
    uint32_t value_length;       // Tamanho do valor
    uint32_t value_type;         // Tipo: STRING, INT, FLOAT, BLOB
    char*    key;                // Chave (UTF-8)
    void*    value;              // Valor
} cafe_metadata_entry_t;
```

**Metadados Padrão**:
- `dataset.name`: Nome do dataset
- `dataset.version`: Versão do dataset
- `dataset.description`: Descrição
- `dataset.source`: Origem dos dados
- `dataset.license`: Licença
- `processing.pipeline`: Pipeline usado
- `processing.timestamp`: Data de processamento
- `stats.mean`: Média dos pixels (por canal)
- `stats.std`: Desvio padrão (por canal)
- `stats.min`: Valor mínimo
- `stats.max`: Valor máximo

### 2.4 Image Descriptor

```c
typedef struct {
    // Identificação (32 bytes)
    uint64_t image_id;           // ID único da imagem
    char     filename[24];       // Nome original (truncado)

    // Dimensões (16 bytes)
    uint32_t width;              // Largura em pixels
    uint32_t height;             // Altura em pixels
    uint16_t num_blocks_x;       // Blocos na horizontal
    uint16_t num_blocks_y;       // Blocos na vertical
    uint16_t num_channels;       // Canais (1, 3, 4)
    uint16_t bit_depth;          // Bits por canal (deprecated, use pixel_format)

    // Formato de Pixel e HDR (16 bytes) - NOVO
    uint8_t  pixel_format;       // cafe_pixel_format_t
    uint8_t  colorspace;         // cafe_colorspace_t
    uint8_t  transfer_function;  // cafe_transfer_function_t
    uint8_t  is_hdr;             // 1 se HDR, 0 se LDR
    float    white_point;        // Ponto branco para HDR (e.g., 1.0, 100.0, 10000.0)
    float    black_point;        // Ponto preto para HDR (normalmente 0.0)
    uint32_t reserved_hdr;       // Reservado para expansão HDR

    // Compressão (24 bytes)
    uint32_t compression_type;   // ZSTD, FSE, NONE, etc.
    uint32_t predictor_type;     // Tipo de preditor usado
    uint32_t original_size;      // Tamanho descomprimido
    uint32_t compressed_size;    // Tamanho comprimido
    float    compression_ratio;  // Razão de compressão
    uint32_t reserved1;

    // Offsets e Índices (32 bytes)
    uint64_t block_index_offset; // Offset na Block Index Table
    uint64_t data_offset;        // Offset dos dados da imagem
    uint64_t metadata_offset;    // Offset de metadados específicos
    uint64_t thumbnail_offset;   // Offset do thumbnail (0 se não houver)

    // Checksums (40 bytes)
    uint32_t header_crc32;       // CRC-32 deste descriptor
    uint8_t  data_sha256[32];    // SHA-256 dos dados da imagem
    uint32_t reserved2;

} cafe_image_descriptor_t;  // Total: 176 bytes (atualizado de 160)
```

**Nota**: O tamanho do descriptor aumentou de 160 para 176 bytes para acomodar os campos HDR. Para manter compatibilidade retroativa, versões antigas podem ignorar os últimos 16 bytes.

### 2.5 Block Structure

```c
typedef struct {
    // Block Header (32 bytes)
    uint32_t magic;              // "CBLK" (0x43424C4B)
    uint16_t block_x;            // Posição X do bloco
    uint16_t block_y;            // Posição Y do bloco
    uint16_t width;              // Largura efetiva (≤128)
    uint16_t height;             // Altura efetiva (≤128)
    uint8_t  num_channels;       // Canais neste bloco
    uint8_t  bit_depth;          // Bits por canal
    uint8_t  compression_type;   // Codec usado
    uint8_t  predictor_type;     // Preditor usado
    uint32_t uncompressed_size;  // Tamanho original
    uint32_t compressed_size;    // Tamanho comprimido
    uint32_t block_crc32;        // CRC-32 do bloco
    uint32_t reserved;

    // Block Data (variável)
    uint8_t  data[];             // Dados comprimidos

} cafe_block_t;
```

### 2.6 HDR (High Dynamic Range) Support

CAFE suporta nativamente imagens HDR para atender casos de uso profissionais, científicos e de machine learning avançado.

#### 2.6.1 Casos de Uso HDR

**Renderização 3D e Computer Graphics**:
- Rendering fotorrealístico com iluminação física
- PBR (Physically Based Rendering) workflows
- Compositing e VFX
- Tone mapping e color grading

**Fotografia Computacional**:
- HDR bracketing e merging
- Multi-exposure fusion
- Tone mapping neural
- Image-to-HDR synthesis

**Imaging Científico**:
- Astronomy (telescópios, satélites)
- Medical imaging (CT, MRI com range dinâmico alto)
- Microscopia de fluorescência
- Espectroscopia

**Machine Learning**:
- Treinamento de modelos de tone mapping
- Relighting neural
- Image enhancement baseado em HDR
- Inverse rendering

#### 2.6.2 Formatos HDR Suportados

**FLOAT16 (Half-Precision)**:
```c
// 16-bit IEEE 754 half-precision float
// 1 bit sinal, 5 bits exponent, 10 bits mantissa
// Range: ±65504, Precisão: ~3 dígitos decimais

typedef struct {
    uint16_t bits;
    // Decodifica para: (-1)^sign × 2^(exp-15) × (1 + mantissa/1024)
} float16_t;

// Exemplos de valores:
// 0.0      = 0x0000
// 1.0      = 0x3C00
// 100.0    = 0x5640
// 10000.0  = 0x70E2 (fora do range normal)
```

**Vantagens**:
- Tamanho compacto (2 bytes por canal)
- Suporte nativo em GPUs modernas
- Suficiente para maioria dos casos HDR
- Compressão razoável com ZSTD (~2:1)

**Limitações**:
- Range limitado a ±65504
- Precisão de ~3 dígitos significativos
- Não adequado para cálculos científicos de alta precisão

**FLOAT32 (Single-Precision)**:
```c
// 32-bit IEEE 754 single-precision float
// 1 bit sinal, 8 bits exponent, 23 bits mantissa
// Range: ±3.4×10^38, Precisão: ~7 dígitos decimais

typedef float float32_t;  // Standard C float
```

**Vantagens**:
- Range virtualmente ilimitado
- Precisão adequada para qualquer aplicação
- Padrão em formatos científicos (OpenEXR, FITS)
- Aritmética nativa em todas as plataformas

**Limitações**:
- 4 bytes por canal (4× maior que UINT8)
- Compressão limitada (~1.5:1 com ZSTD)
- Pode requerer compressão especializada

#### 2.6.3 Espaços de Cor e Transfer Functions

**Linear RGB** (padrão para HDR):
```
Valores representam luminância física diretamente
L_display = pixel_value
Ideal para: rendering, compositing, cálculos físicos
```

**sRGB** (padrão para LDR):
```
Transfer function não-linear (~gamma 2.2)
L_display = pixel_value^2.2 (aproximado)
Ideal para: web, displays consumer
```

**PQ (Perceptual Quantizer - HDR10)**:
```
SMPTE ST 2084, otimizado para percepção humana
Range: 0 - 10,000 nits
Usado em: HDR10, HDR10+, Dolby Vision
```

**HLG (Hybrid Log-Gamma)**:
```
Rec. 2100, compatível com SDR
Range: 0 - 1,000 nits
Usado em: broadcast HDR
```

#### 2.6.4 Metadata HDR

```c
typedef struct {
    // Luminância
    float max_luminance;         // cd/m² (nits), e.g. 1000, 4000, 10000
    float min_luminance;         // cd/m² (nits), e.g. 0.001, 0.0001
    float avg_luminance;         // Luminância média da imagem

    // Chromaticity (CIE 1931)
    float white_point_x;         // D65: 0.3127
    float white_point_y;         // D65: 0.3290
    float red_primary_x;         // Primária vermelha
    float red_primary_y;
    float green_primary_x;       // Primária verde
    float green_primary_y;
    float blue_primary_x;        // Primária azul
    float blue_primary_y;

    // Mastering Display (para HDR10 metadata)
    float mastering_display_min_lum;
    float mastering_display_max_lum;

} cafe_hdr_metadata_t;
```

**Armazenamento**: Metadata HDR é salvo na seção Global Metadata com chave `hdr.metadata`.

#### 2.6.5 Workflow HDR Típico

**Criação**:
```python
import cafe
import numpy as np

# Carregar HDR (OpenEXR, Radiance, etc.)
hdr_image = load_exr('scene.exr')  # Float32, linear RGB

# Salvar em CAFE
with cafe.create('scene.cafe') as f:
    f.add_image(
        hdr_image,
        pixel_format=cafe.PIXEL_FLOAT32,
        colorspace=cafe.COLORSPACE_LINEAR,
        white_point=100.0,  # 100 nits
        hdr_metadata={
            'max_luminance': 10000.0,
            'min_luminance': 0.001,
            'white_point_x': 0.3127,  # D65
            'white_point_y': 0.3290,
        }
    )
```

**Leitura e Tone Mapping**:
```python
# Carregar HDR
with cafe.open('scene.cafe') as f:
    hdr_image = f.read_image(0)  # Float32 linear
    metadata = f.get_hdr_metadata(0)

# Tone mapping para display
ldr_image = tonemap_reinhard(hdr_image,
                              white_point=metadata['white_point'])

# Ou usar tone mapping neural
ldr_image = neural_tonemap_model(hdr_image)
```

#### 2.6.6 Compressão de Dados HDR

**Desafios**:
- Floats têm padrão de bits menos regular que integers
- ZSTD padrão não comprime bem (~1.3-1.8:1)
- Requer técnicas especializadas

**Estratégias de Compressão**:

1. **Quantização Reversível**:
```c
// Quantizar mantissa para reduzir entropia
float16_t quantize_mantissa(float16_t value, int bits) {
    uint16_t v = value.bits;
    uint16_t mask = ~((1 << (10 - bits)) - 1);  // Preservar N bits
    return (float16_t){.bits = v & mask};
}
```

2. **Predição de Valores**:
```c
// Preditor diferencial também funciona em floats
void predict_hdr_block(float* block, int size) {
    for (int i = size - 1; i > 0; i--) {
        block[i] -= block[i - 1];  // Diferença
    }
}
// Comprime melhor pois diferenças têm menor magnitude
```

3. **Compressão Adaptativa**:
```c
// Detectar se valores são pequenos (alto expoente negativo)
if (all_values_small(block)) {
    // Converter para fixed-point e comprimir como integer
    compress_as_fixed_point(block);
} else {
    // Compressão padrão ZSTD
    compress_zstd(block);
}
```

4. **Compressão Lossy Opcional** (fase futura):
```c
typedef struct {
    float quantization_step;     // Step de quantização
    float max_error;             // Erro máximo permitido
    int   preserve_range;        // Preservar range dinâmico
} cafe_hdr_lossy_params_t;

// Quantizar valores mantendo range
float quantize_hdr(float value, float step) {
    return roundf(value / step) * step;
}
```

**Resultados Esperados**:
- FLOAT16: ~2:1 com predictors
- FLOAT32: ~1.8:1 com predictors
- FLOAT32 científico (muito irregular): ~1.3:1

#### 2.6.7 Conversões HDR ↔ LDR

**HDR → LDR (Tone Mapping)**:
```c
// Reinhard global tone mapping
uint8_t tonemap_reinhard(float hdr_value, float white_point) {
    float l = hdr_value / white_point;
    float mapped = l / (1.0f + l);  // [0, 1]

    // Aplicar gamma sRGB
    mapped = powf(mapped, 1.0f/2.2f);

    return (uint8_t)(mapped * 255.0f);
}
```

**LDR → HDR (Inverse Tone Mapping)**:
```c
// Estimativa simples (limitada)
float inverse_tonemap_simple(uint8_t ldr_value, float white_point) {
    float srgb = ldr_value / 255.0f;

    // Remover gamma
    float linear = powf(srgb, 2.2f);

    // Inverse Reinhard (aproximado)
    float hdr = (linear / (1.0f - linear)) * white_point;

    return hdr;
}
```

**Nota**: Inverse tone mapping é inerentemente mal-posto. Métodos baseados em ML produzem resultados muito superiores.

---

## 3. Compressão e Codecs

### 3.1 Codecs Suportados

#### 3.1.1 Zstandard (ZSTD) - Primary Codec

**Características**:
- Razão de compressão: 2.5x - 3.5x (média)
- Velocidade: 400-500 MB/s (descompressão)
- Níveis: 1-22 (padrão: 3 para treinamento, 19 para armazenamento)

**Configuração**:
```c
typedef struct {
    int compression_level;       // 1-22
    int enable_dict;             // Usar dicionário compartilhado
    int window_log;              // Tamanho da janela (10-31)
    int enable_ldm;              // Long Distance Matching
    int nb_workers;              // Threads para compressão paralela
} zstd_params_t;
```

#### 3.1.2 Finite State Entropy (FSE) - Secondary Codec

**Uso**: Blocos com baixa entropia (gradientes suaves, áreas uniformes)

**Características**:
- Razão de compressão: 3x - 5x (em dados adequados)
- Velocidade: 800-1000 MB/s (descompressão)
- Overhead: Mínimo (~2% do tamanho)

#### 3.1.3 NONE - Sem Compressão

**Uso**: Blocos onde compressão é contraproducente (ruído, alta entropia)

**Decisão automática**:
```python
def select_codec(block_data):
    entropy = calculate_entropy(block_data)

    if entropy > 0.95:
        return CODEC_NONE  # Alta entropia - não comprime
    elif entropy < 0.3:
        return CODEC_FSE   # Baixa entropia - FSE eficiente
    else:
        return CODEC_ZSTD  # Caso geral
```

### 3.2 Predictors (Filtros Pré-Compressão)

#### 3.2.1 Differential Predictor (Implementado)

```c
// Predição: P[x,y] = Pixel[x-1, y]
// Residual: R[x,y] = Pixel[x,y] - P[x,y]
void apply_differential_predictor(uint8_t* block, int width, int height) {
    for (int y = 0; y < height; y++) {
        for (int x = width - 1; x > 0; x--) {
            block[y * width + x] -= block[y * width + x - 1];
        }
    }
}
```

#### 3.2.2 Median Edge Detector (MED) - Planejado

```c
// P[x,y] = median(Pixel[x-1,y], Pixel[x,y-1], Pixel[x-1,y] + Pixel[x,y-1] - Pixel[x-1,y-1])
uint8_t predict_med(uint8_t left, uint8_t top, uint8_t top_left) {
    int prediction = left + top - top_left;
    int range_min = left < top ? left : top;
    int range_max = left > top ? left : top;

    if (prediction < range_min) return range_min;
    if (prediction > range_max) return range_max;
    return prediction;
}
```

#### 3.2.3 Paeth Predictor (PNG-style) - Planejado

```c
// Preditor usado em PNG
uint8_t paeth_predictor(uint8_t a, uint8_t b, uint8_t c) {
    int p = a + b - c;
    int pa = abs(p - a);
    int pb = abs(p - b);
    int pc = abs(p - c);

    if (pa <= pb && pa <= pc) return a;
    else if (pb <= pc) return b;
    else return c;
}
```

#### 3.2.4 AI-Powered Predictor - Opcional

**Conceito**: Usar uma rede neural leve para predição de pixels

```python
class NeuralPredictor(nn.Module):
    def __init__(self):
        super().__init__()
        self.conv1 = nn.Conv2d(channels, 64, 3, padding=1)
        self.conv2 = nn.Conv2d(64, 64, 3, padding=1)
        self.conv3 = nn.Conv2d(64, channels, 1)

    def forward(self, context):
        # Context: 5x5 janela ao redor do pixel
        x = F.relu(self.conv1(context))
        x = F.relu(self.conv2(x))
        prediction = self.conv3(x)
        return prediction
```

**Trade-off**: Melhor compressão vs. overhead de processamento

---

## 4. AI Metadata System

### 4.1 Chunk Structure

```c
typedef struct {
    uint32_t magic;              // "AIMT" (0x41494D54)
    uint32_t chunk_type;         // EMBEDDING, LABEL, MASK, FEATURE, etc.
    uint64_t image_id;           // ID da imagem associada
    uint32_t data_size;          // Tamanho dos dados
    uint32_t data_format;        // Formato (FP32, FP16, INT8, etc.)
    uint32_t dimensions[4];      // Dimensões do tensor [N,C,H,W]
    uint32_t compression;        // NONE, ZSTD, QUANTIZED
    uint32_t checksum;           // CRC-32
    uint8_t  data[];             // Dados do metadata
} cafe_ai_metadata_chunk_t;
```

### 4.2 Tipos de Metadata

#### 4.2.1 Image Embeddings

**Formato**: Vetor de features de modelos pré-treinados

```c
typedef struct {
    uint16_t embedding_dim;      // Dimensão (e.g., 512, 1024, 2048)
    uint8_t  model_type;         // RESNET, VIT, CLIP, etc.
    uint8_t  precision;          // FP32, FP16, INT8
    float*   embedding_vector;   // Vetor de features
} cafe_embedding_t;
```

**Modelos suportados**:
- ResNet-50: 2048-dim
- Vision Transformer: 768-dim
- CLIP: 512-dim
- Custom models: dimensão arbitrária

#### 4.2.2 Classification Labels

```c
typedef struct {
    uint32_t num_classes;        // Número de classes
    uint32_t num_labels;         // Labels nesta imagem
    struct {
        uint32_t class_id;       // ID da classe
        float    confidence;     // Confiança [0,1]
        char     class_name[64]; // Nome da classe
    } labels[];
} cafe_classification_labels_t;
```

#### 4.2.3 Segmentation Masks

```c
typedef struct {
    uint32_t mask_width;         // Largura da máscara
    uint32_t mask_height;        // Altura da máscara
    uint32_t num_classes;        // Classes na segmentação
    uint8_t  compression_type;   // RLE, ZSTD, NONE
    uint32_t compressed_size;
    uint8_t* mask_data;          // Dados da máscara
} cafe_segmentation_mask_t;
```

**Compressão de máscaras**: Run-Length Encoding (RLE) para máscaras esparsas

#### 4.2.4 Bounding Boxes (Object Detection)

```c
typedef struct {
    uint32_t num_boxes;          // Número de caixas
    struct {
        float    x, y, w, h;     // Coordenadas normalizadas [0,1]
        uint32_t class_id;       // Classe do objeto
        float    confidence;     // Confiança
        char     class_name[32];
    } boxes[];
} cafe_bounding_boxes_t;
```

#### 4.2.5 Keypoints (Pose Estimation)

```c
typedef struct {
    uint32_t num_keypoints;      // Número de keypoints
    struct {
        float    x, y;           // Coordenadas normalizadas
        float    confidence;     // Visibilidade/confiança
        uint16_t keypoint_id;    // ID do keypoint
    } keypoints[];
} cafe_keypoints_t;
```

### 4.3 Metadata Query API

```c
// Buscar embeddings por similaridade
cafe_image_id_t* cafe_query_similar_embeddings(
    cafe_file_t* file,
    float* query_embedding,
    int top_k,
    float threshold
);

// Buscar imagens por label
cafe_image_id_t* cafe_query_by_label(
    cafe_file_t* file,
    const char* label,
    float min_confidence
);

// Buscar por características de segmentação
cafe_image_id_t* cafe_query_by_mask_overlap(
    cafe_file_t* file,
    cafe_segmentation_mask_t* query_mask,
    float min_iou
);
```

---

## 5. Aceleração GPU

### 5.1 CUDA Integration

#### 5.1.1 nvCOMP Decompression

```cuda
// Descompressão batch em GPU
__global__ void decompress_blocks_kernel(
    uint8_t** compressed_blocks,
    size_t* compressed_sizes,
    uint8_t** output_buffers,
    size_t* output_sizes,
    int num_blocks
) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < num_blocks) {
        nvcompZstdDecompressAsync(
            compressed_blocks[idx],
            compressed_sizes[idx],
            output_buffers[idx],
            output_sizes[idx],
            stream
        );
    }
}
```

#### 5.1.2 Batch Loading Pipeline

```c
typedef struct {
    // GPU buffers
    uint8_t* d_compressed_data;   // Dados comprimidos na GPU
    uint8_t* d_decompressed_data; // Dados descomprimidos na GPU

    // CUDA streams para overlap
    cudaStream_t transfer_stream;
    cudaStream_t decompress_stream;
    cudaStream_t process_stream;

    // Batch configuration
    int batch_size;
    int block_size;
} cafe_gpu_context_t;

// Pipeline:
// 1. Transfer compressed data to GPU (stream 1)
// 2. Decompress on GPU (stream 2)
// 3. Process/transform (stream 3)
void cafe_gpu_batch_load(cafe_gpu_context_t* ctx, int* image_ids, int count);
```

### 5.2 Performance Optimizations

#### 5.2.1 Pinned Memory

```c
// Alocar memória pinned para transferências rápidas
void* cafe_alloc_pinned(size_t size) {
    void* ptr;
    cudaHostAlloc(&ptr, size, cudaHostAllocDefault);
    return ptr;
}
```

#### 5.2.2 Prefetching

```c
typedef struct {
    int* next_batch_ids;
    int  next_batch_size;
    pthread_t prefetch_thread;
} cafe_prefetch_context_t;

// Thread de prefetch assíncrono
void* prefetch_worker(void* arg) {
    cafe_prefetch_context_t* ctx = arg;
    while (running) {
        cafe_gpu_batch_load(gpu_ctx, ctx->next_batch_ids, ctx->next_batch_size);
    }
}
```

---

## 6. Streaming e Progressive Decoding

### 6.1 Hierarchical Levels

**Level 0**: Thumbnail (1/16 da resolução)
**Level 1**: Preview (1/4 da resolução)
**Level 2**: Full Resolution

```c
typedef struct {
    uint8_t  num_levels;         // Número de níveis
    struct {
        uint16_t width;
        uint16_t height;
        uint32_t data_offset;
        uint32_t data_size;
    } levels[MAX_LEVELS];
} cafe_progressive_info_t;
```

### 6.2 HTTP Range Requests

```c
// Suporte a byte-range requests para streaming web
typedef struct {
    uint64_t start_byte;
    uint64_t end_byte;
    cafe_file_t* file;
} cafe_range_request_t;

int cafe_serve_range(cafe_range_request_t* request, uint8_t* output);
```

### 6.3 Adaptive Streaming

```c
// Selecionar nível baseado em bandwidth
int cafe_select_quality_level(
    int available_bandwidth_mbps,
    int target_latency_ms,
    cafe_progressive_info_t* info
) {
    // Algoritmo adaptativo (similar a DASH/HLS)
    if (available_bandwidth_mbps > 50) return 2;  // Full quality
    if (available_bandwidth_mbps > 10) return 1;  // Preview
    return 0;  // Thumbnail
}
```

---

## 7. API Reference

### 7.1 Core C API

#### 7.1.1 File Operations

```c
// Abrir arquivo CAFE
cafe_file_t* cafe_open(const char* path, const char* mode);

// Fechar arquivo
int cafe_close(cafe_file_t* file);

// Criar novo arquivo
cafe_file_t* cafe_create(const char* path, cafe_config_t* config);

// Ler header
int cafe_read_header(cafe_file_t* file, cafe_file_header_t* header);

// Escrever header
int cafe_write_header(cafe_file_t* file, cafe_file_header_t* header);
```

#### 7.1.2 Image Operations

```c
// Adicionar imagem ao arquivo
int cafe_add_image(
    cafe_file_t* file,
    uint8_t* pixel_data,
    int width,
    int height,
    int channels,
    cafe_compression_params_t* params
);

// Ler imagem completa
int cafe_read_image(
    cafe_file_t* file,
    uint64_t image_id,
    uint8_t** pixel_data,
    cafe_image_descriptor_t* descriptor
);

// Ler apenas metadados da imagem
int cafe_read_image_descriptor(
    cafe_file_t* file,
    uint64_t image_id,
    cafe_image_descriptor_t* descriptor
);
```

#### 7.1.3 Block-Level Operations

```c
// Ler bloco específico
int cafe_read_block(
    cafe_file_t* file,
    uint64_t image_id,
    int block_x,
    int block_y,
    cafe_block_t** block
);

// Ler batch de blocos
int cafe_read_blocks_batch(
    cafe_file_t* file,
    cafe_block_request_t* requests,
    int num_requests,
    cafe_block_t** blocks
);

// Escrever bloco
int cafe_write_block(
    cafe_file_t* file,
    cafe_block_t* block
);
```

#### 7.1.4 Metadata Operations

```c
// Adicionar metadata global
int cafe_add_global_metadata(
    cafe_file_t* file,
    const char* key,
    const void* value,
    uint32_t value_type,
    uint32_t value_size
);

// Ler metadata global
int cafe_get_global_metadata(
    cafe_file_t* file,
    const char* key,
    void** value,
    uint32_t* value_type,
    uint32_t* value_size
);

// Adicionar AI metadata
int cafe_add_ai_metadata(
    cafe_file_t* file,
    uint64_t image_id,
    cafe_ai_metadata_chunk_t* metadata
);

// Ler AI metadata
int cafe_get_ai_metadata(
    cafe_file_t* file,
    uint64_t image_id,
    uint32_t chunk_type,
    cafe_ai_metadata_chunk_t** metadata
);
```

#### 7.1.5 HDR Operations

```c
// Adicionar imagem HDR (float16)
int cafe_add_image_hdr16(
    cafe_file_t* file,
    float16_t* pixel_data,      // Half-precision floats
    int width,
    int height,
    int channels,
    cafe_hdr_params_t* hdr_params
);

// Adicionar imagem HDR (float32)
int cafe_add_image_hdr32(
    cafe_file_t* file,
    float* pixel_data,          // Single-precision floats
    int width,
    int height,
    int channels,
    cafe_hdr_params_t* hdr_params
);

// Ler imagem HDR (retorna formato original)
int cafe_read_image_hdr(
    cafe_file_t* file,
    uint64_t image_id,
    void** pixel_data,          // float16_t* ou float*
    cafe_pixel_format_t* format,
    cafe_hdr_metadata_t* hdr_metadata
);

// Converter LDR para HDR ao ler (inverse tone mapping simples)
int cafe_read_image_as_hdr(
    cafe_file_t* file,
    uint64_t image_id,
    float** pixel_data_fp32,
    float white_point
);

// Converter HDR para LDR ao ler (tone mapping)
int cafe_read_hdr_image_as_ldr(
    cafe_file_t* file,
    uint64_t image_id,
    uint8_t** pixel_data_uint8,
    cafe_tonemap_params_t* tonemap_params
);

// Parâmetros HDR
typedef struct {
    cafe_pixel_format_t pixel_format;    // FLOAT16 ou FLOAT32
    cafe_colorspace_t colorspace;        // LINEAR, REC2020, etc.
    cafe_transfer_function_t transfer;   // LINEAR, PQ, HLG
    float white_point;                   // Nits
    float black_point;                   // Nits
    cafe_hdr_metadata_t* metadata;       // Opcional
} cafe_hdr_params_t;

// Parâmetros de tone mapping
typedef enum {
    CAFE_TONEMAP_REINHARD,
    CAFE_TONEMAP_FILMIC,
    CAFE_TONEMAP_ACES,
    CAFE_TONEMAP_HABLE,
} cafe_tonemap_operator_t;

typedef struct {
    cafe_tonemap_operator_t operator;
    float white_point;
    float exposure;
    float gamma;
} cafe_tonemap_params_t;
```

**Exemplo de Uso - Salvar HDR**:
```c
// Carregar OpenEXR (usando biblioteca externa)
float* hdr_pixels = load_exr("scene.exr", &width, &height, &channels);

// Configurar parâmetros HDR
cafe_hdr_params_t hdr_params = {
    .pixel_format = CAFE_PIXEL_FLOAT32,
    .colorspace = CAFE_COLORSPACE_LINEAR,
    .transfer = CAFE_TRANSFER_LINEAR,
    .white_point = 100.0f,  // 100 nits
    .black_point = 0.0f,
    .metadata = &hdr_metadata
};

// Salvar em CAFE
cafe_file_t* file = cafe_create("scene.cafe", NULL);
cafe_add_image_hdr32(file, hdr_pixels, width, height, channels, &hdr_params);
cafe_close(file);
```

**Exemplo de Uso - Ler e Tone Map**:
```c
// Abrir arquivo HDR
cafe_file_t* file = cafe_open("scene.cafe", "rb");

// Tone mapping para LDR
cafe_tonemap_params_t tonemap = {
    .operator = CAFE_TONEMAP_ACES,
    .white_point = 100.0f,
    .exposure = 1.0f,
    .gamma = 2.2f
};

uint8_t* ldr_pixels;
cafe_read_hdr_image_as_ldr(file, 0, &ldr_pixels, &tonemap);

// Salvar como PNG
save_png("output.png", ldr_pixels, width, height, channels);

free(ldr_pixels);
cafe_close(file);
```

### 7.2 Python API (Bindings via CFFI)

```python
import cafe

# Abrir arquivo
with cafe.open("dataset.cafe") as f:
    # Ler informações do header
    header = f.header
    print(f"Total de imagens: {header.total_images}")

    # Iterar sobre imagens
    for img in f:
        pixels = img.pixels  # NumPy array
        labels = img.metadata.labels
        embedding = img.metadata.embedding

    # Busca por similaridade
    similar = f.query_similar(embedding_vector, top_k=10)

    # Busca por label
    cats = f.query_by_label("cat", min_confidence=0.8)

# Criar novo arquivo
with cafe.create("output.cafe") as f:
    for image_path in image_list:
        img = load_image(image_path)
        f.add_image(
            img,
            metadata={
                "label": "cat",
                "embedding": extract_features(img),
                "source": image_path
            }
        )

# Trabalhar com HDR
import cafe
import numpy as np
import OpenEXR  # ou outra biblioteca HDR

# Criar arquivo HDR
with cafe.create("hdr_dataset.cafe") as f:
    # Carregar imagem HDR
    hdr_image = load_exr("sunset.exr")  # Float32 array, shape (H,W,3)

    f.add_image(
        hdr_image,
        pixel_format=cafe.PIXEL_FLOAT32,
        colorspace=cafe.COLORSPACE_LINEAR,
        white_point=100.0,  # 100 nits
        hdr_metadata={
            'max_luminance': 10000.0,
            'min_luminance': 0.001,
            'mastering_display': 'P3-D65'
        }
    )

# Ler HDR e fazer tone mapping
with cafe.open("hdr_dataset.cafe") as f:
    # Ler como HDR (float32)
    hdr_img = f.read_image(0, as_hdr=True)
    print(f"HDR shape: {hdr_img.shape}, dtype: {hdr_img.dtype}")
    print(f"Value range: [{hdr_img.min():.3f}, {hdr_img.max():.3f}]")

    # Tone mapping automático
    ldr_img = f.read_image(0,
                          tonemap='aces',
                          exposure=1.0,
                          gamma=2.2)

    # Ou manual
    hdr_metadata = f.get_hdr_metadata(0)
    ldr_img = tonemap_reinhard(hdr_img,
                                white_point=hdr_metadata['white_point'])

# Conversão HDR ↔ LDR
ldr_dataset = cafe.open("imagenet_ldr.cafe")
hdr_dataset = cafe.create("imagenet_pseudo_hdr.cafe")

for img_id in range(len(ldr_dataset)):
    ldr_img = ldr_dataset.read_image(img_id)  # uint8

    # Inverse tone mapping (limitado, melhor usar modelo ML)
    pseudo_hdr = inverse_tonemap_simple(ldr_img, white_point=1.0)

    # Ou usar modelo neural
    pseudo_hdr = neural_inverse_tonemap_model(ldr_img)

    hdr_dataset.add_image(pseudo_hdr,
                         pixel_format=cafe.PIXEL_FLOAT16,
                         colorspace=cafe.COLORSPACE_LINEAR)
```

### 7.3 ML Framework Integration

#### 7.3.1 PyTorch DataLoader

```python
from cafe.torch import CAFEDataset
from torch.utils.data import DataLoader

# Dataset
dataset = CAFEDataset(
    "dataset.cafe",
    transform=transforms.Compose([
        transforms.ToTensor(),
        transforms.Normalize(mean=[0.485, 0.456, 0.406],
                           std=[0.229, 0.224, 0.225])
    ])
)

# DataLoader com workers paralelos
loader = DataLoader(
    dataset,
    batch_size=32,
    num_workers=4,
    pin_memory=True,  # Para transferência GPU rápida
    prefetch_factor=2
)

# Treinamento
for batch_idx, (images, labels, metadata) in enumerate(loader):
    images = images.cuda()
    labels = labels.cuda()

    outputs = model(images)
    loss = criterion(outputs, labels)
    # ...
```

#### 7.3.2 TensorFlow Dataset

```python
import tensorflow as tf
from cafe.tensorflow import CAFEDatasetBuilder

# Criar TF Dataset
dataset = CAFEDatasetBuilder("dataset.cafe") \
    .map(preprocess_function) \
    .batch(32) \
    .prefetch(tf.data.AUTOTUNE) \
    .build()

# Treinamento
model.fit(
    dataset,
    epochs=10,
    callbacks=[...]
)
```

---

## 8. Benchmarks e Metas de Performance

### 8.1 Cenários de Teste

#### 8.1.1 Batch Loading (1000 imagens)

**Baseline (PNG)**:
- Tempo: ~5.2 segundos
- Throughput: ~192 imagens/s
- IOPS: ~4000 reads

**Meta CAFE**:
- Tempo: ~1.5 segundos (3.5× mais rápido)
- Throughput: ~666 imagens/s
- IOPS: ~100 reads (container único)

#### 8.1.2 GPU Decompression

**CPU Baseline (ZSTD)**:
- Throughput: ~450 MB/s (single thread)
- Latência: ~2.2ms por imagem (1MB)

**Meta GPU (nvCOMP)**:
- Throughput: ~4500 MB/s (10× mais rápido)
- Latência: ~0.2ms por imagem (batch)

#### 8.1.3 Storage Efficiency

**PNG (lossless)**:
- Tamanho médio: 1.0× (baseline)
- Razão de compressão: ~2.5:1

**Meta CAFE**:
- Tamanho médio: 0.7-0.9× (10-30% melhor)
- Razão de compressão: ~3.2:1

#### 8.1.4 Memory Usage

**Arquivos individuais**:
- Overhead: ~4KB por arquivo (metadata do filesystem)
- 10.000 imagens = 40MB overhead

**CAFE Container**:
- Overhead: ~256 bytes (header) + ~160 bytes/imagem (descriptor)
- 10.000 imagens = ~1.6MB overhead
- **Redução: 96% no overhead**

### 8.2 Datasets de Benchmark

1. **ImageNet-1K** (1.28M imagens)
   - Resolução média: 469×387
   - Tamanho total (PNG): ~144 GB
   - Meta CAFE: ~110 GB

2. **COCO 2017** (118K imagens + máscaras)
   - Resolução média: 640×480
   - Tamanho total: ~25 GB
   - Meta CAFE: ~19 GB (incluindo máscaras integradas)

3. **Medical Imaging** (16-bit grayscale)
   - Resolução: 512×512, 1024×1024
   - Meta: 40% melhor que DICOM

---

## 9. Verificação de Integridade

### 9.1 Checksums em Múltiplos Níveis

```c
// Nível 1: CRC-32 por bloco (rápido)
uint32_t cafe_compute_block_crc(cafe_block_t* block);

// Nível 2: SHA-256 por imagem (médio)
void cafe_compute_image_hash(cafe_image_descriptor_t* desc, uint8_t* hash);

// Nível 3: SHA-256 do arquivo inteiro (lento, completo)
void cafe_compute_file_hash(cafe_file_t* file, uint8_t* hash);
```

### 9.2 Error Recovery

```c
typedef struct {
    int num_errors;
    struct {
        uint64_t image_id;
        int block_x;
        int block_y;
        uint32_t expected_crc;
        uint32_t actual_crc;
    } errors[MAX_ERRORS];
} cafe_integrity_report_t;

// Verificar integridade e tentar recuperação
cafe_integrity_report_t* cafe_verify_and_repair(
    cafe_file_t* file,
    int repair_mode  // 0=report, 1=auto-repair, 2=interactive
);
```

### 9.3 Redundância (Opcional)

```c
// Reed-Solomon error correction para blocos críticos
typedef struct {
    int enable_ecc;
    int parity_blocks;       // Blocos de paridade (padrão: 2)
    int recovery_threshold;  // Mínimo para recuperação
} cafe_ecc_params_t;
```

---

## 10. Ferramentas e Utilidades

### 10.1 Conversor (cafe-convert)

```bash
# Converter imagens para CAFE
cafe-convert \
  --input /path/to/images/*.png \
  --output dataset.cafe \
  --compression zstd \
  --level 3 \
  --threads 8 \
  --predictor differential

# Converter CAFE para imagens
cafe-convert \
  --input dataset.cafe \
  --output /path/to/output \
  --format png
```

### 10.2 Inspetor (cafe-inspect)

```bash
# Ver informações do arquivo
cafe-inspect dataset.cafe

# Saída:
# CAFE Format v1.0
# ═══════════════════════════════════════
# File size: 1.2 GB
# Total images: 10,000
# Total blocks: 78,125
# Compression: ZSTD (level 3)
# Compression ratio: 3.2:1
#
# Metadata:
#   dataset.name: ImageNet-Subset
#   dataset.version: 1.0
#
# Block distribution:
#   ZSTD: 75,234 (96.3%)
#   FSE: 2,451 (3.1%)
#   NONE: 440 (0.6%)

# Verificar integridade
cafe-inspect dataset.cafe --verify
```

### 10.3 Benchmark Tool (cafe-bench)

```bash
# Benchmark de leitura sequencial
cafe-bench \
  --file dataset.cafe \
  --mode sequential \
  --threads 4 \
  --iterations 100

# Benchmark de leitura aleatória
cafe-bench \
  --file dataset.cafe \
  --mode random \
  --count 1000 \
  --gpu

# Comparar com PNG
cafe-bench \
  --cafe dataset.cafe \
  --baseline /path/to/pngs \
  --report benchmark_report.html
```

### 10.4 Metadata Manager (cafe-meta)

```bash
# Adicionar embeddings em batch
cafe-meta add-embeddings \
  --file dataset.cafe \
  --model resnet50 \
  --checkpoint model.pth \
  --batch-size 32 \
  --gpu

# Exportar metadata
cafe-meta export \
  --file dataset.cafe \
  --output metadata.json \
  --format json

# Buscar por similaridade
cafe-meta query \
  --file dataset.cafe \
  --image query.jpg \
  --top-k 10 \
  --output similar.txt
```

---

## 11. Extensões Futuras (Fase 3+)

### 11.1 Dataset Versioning

```c
typedef struct {
    uint32_t version_number;
    uint64_t parent_version;     // 0 se primeira versão
    uint64_t timestamp;
    char     description[256];

    // Delta encoding: apenas mudanças
    uint64_t* added_images;      // IDs de imagens adicionadas
    uint64_t* removed_images;    // IDs de imagens removidas
    uint64_t* modified_images;   // IDs de imagens modificadas

} cafe_version_t;
```

### 11.2 Federated Learning Support

```c
// Metadados para federated learning
typedef struct {
    uint32_t partition_id;       // ID da partição
    uint32_t total_partitions;   // Total de partições
    uint8_t  privacy_level;      // Nível de privacidade
    uint8_t  encryption_type;    // Tipo de encriptação

    // Differential privacy
    float epsilon;               // Privacy budget
    float delta;                 // Privacy parameter

} cafe_federated_params_t;
```

### 11.3 WebAssembly Decoder

```javascript
// Decodificador CAFE em WASM para browsers
import { CafeDecoder } from 'cafe-wasm';

const decoder = new CafeDecoder();
await decoder.load('dataset.cafe');

// Progressive rendering
for await (const level of decoder.streamProgressive(imageId)) {
    canvas.drawImage(level);  // Level 0, 1, 2...
}

// Worker threads para paralelismo
const worker = new Worker('cafe-decoder-worker.js');
worker.postMessage({ imageId: 42 });
```

### 11.4 Cloud Storage Optimization

```c
// Integração com S3/GCS/Azure Blob
typedef struct {
    char*    cloud_url;          // s3://bucket/dataset.cafe
    int      enable_caching;     // Cache local
    int      prefetch_blocks;    // Número de blocos para prefetch
    uint64_t cache_size_mb;      // Tamanho do cache
} cafe_cloud_config_t;

cafe_file_t* cafe_open_cloud(cafe_cloud_config_t* config);
```

---

## 12. Considerações de Segurança

### 12.1 Validação de Entrada

```c
// Validar header antes de processar
int cafe_validate_header(cafe_file_header_t* header) {
    // Magic number
    if (memcmp(header->magic, "CAFE", 4) != 0) return INVALID_MAGIC;

    // Versão suportada
    if (header->version_major > MAX_SUPPORTED_VERSION) return UNSUPPORTED_VERSION;

    // Sanity checks
    if (header->total_images == 0) return INVALID_IMAGE_COUNT;
    if (header->file_size < sizeof(cafe_file_header_t)) return INVALID_FILE_SIZE;

    // Verificar checksums
    uint32_t computed_crc = crc32(header, HEADER_SIZE - 4);
    if (computed_crc != header->header_crc32) return CHECKSUM_MISMATCH;

    return VALID;
}
```

### 12.2 Buffer Overflow Protection

```c
// Sempre validar tamanhos antes de copiar
int cafe_safe_read_block(cafe_file_t* file, cafe_block_t* block) {
    // Validar tamanhos
    if (block->compressed_size > MAX_BLOCK_SIZE) return ERROR_SIZE_EXCEEDED;
    if (block->uncompressed_size > MAX_UNCOMPRESSED_SIZE) return ERROR_SIZE_EXCEEDED;

    // Alocar com guards
    uint8_t* buffer = safe_malloc(block->compressed_size + GUARD_SIZE);

    // Ler e validar
    fread(buffer, 1, block->compressed_size, file->fp);
    validate_guards(buffer);

    return SUCCESS;
}
```

### 12.3 Sandbox Mode

```c
// Modo sandbox para arquivos não confiáveis
cafe_file_t* cafe_open_sandboxed(const char* path) {
    cafe_file_t* file = cafe_open(path, "rb");

    // Limites rigorosos
    file->max_memory_usage = 100 * 1024 * 1024;  // 100MB
    file->max_decompression_ratio = 100;         // 100:1
    file->enable_validation = 1;                 // Forçar validação
    file->disable_gpu = 1;                       // Sem GPU em sandbox

    return file;
}
```

---

## 13. Licenciamento e Propriedade Intelectual

### 13.1 Licença MIT

O formato CAFE e sua implementação de referência são distribuídos sob licença MIT, permitindo:
- Uso comercial
- Modificação
- Distribuição
- Uso privado

### 13.2 Dependências de Terceiros

- **Zstandard**: BSD-3-Clause / GPLv2 (dual license)
- **FSE**: BSD-2-Clause
- **nvCOMP**: BSD-3-Clause (NVIDIA)

### 13.3 Patentes

O formato CAFE não inclui tecnologias patenteadas. Todos os algoritmos utilizados são de domínio público ou cobertos por licenças permissivas.

---

## 14. Referências e Bibliografia

### 14.1 Compressão

1. Zstandard: https://github.com/facebook/zstd
2. Finite State Entropy: https://github.com/Cyan4973/FiniteStateEntropy
3. Collet, Y., & Kucherawy, M. (2021). "Zstandard Compression and the application/zstd Media Type"

### 14.2 GPU Acceleration

1. nvCOMP: https://github.com/NVIDIA/nvcomp
2. CUDA Programming Guide: https://docs.nvidia.com/cuda/

### 14.3 Image Formats

1. PNG Specification: https://www.w3.org/TR/PNG/
2. JPEG Standard: ITU-T T.81
3. WebP: https://developers.google.com/speed/webp/

### 14.4 Machine Learning I/O

1. TFRecord Format: https://www.tensorflow.org/tutorials/load_data/tfrecord
2. PyTorch DataLoader: https://pytorch.org/docs/stable/data.html

---

## Apêndices

### Apêndice A: Magic Numbers e Identificadores

```c
#define CAFE_MAGIC          0x45464143  // "CAFE"
#define CAFE_BLOCK_MAGIC    0x4B4C4243  // "CBLK"
#define CAFE_METADATA_MAGIC 0x5444454D  // "METD"
#define CAFE_AI_META_MAGIC  0x544D4941  // "AIMT"
#define CAFE_FOOTER_MAGIC   0x52544F46  // "FOTR"
```

### Apêndice B: Códigos de Erro

```c
enum cafe_error_codes {
    CAFE_SUCCESS = 0,
    CAFE_ERROR_INVALID_MAGIC = -1,
    CAFE_ERROR_UNSUPPORTED_VERSION = -2,
    CAFE_ERROR_CORRUPTED_HEADER = -3,
    CAFE_ERROR_CHECKSUM_MISMATCH = -4,
    CAFE_ERROR_DECOMPRESSION_FAILED = -5,
    CAFE_ERROR_OUT_OF_MEMORY = -6,
    CAFE_ERROR_IO_ERROR = -7,
    CAFE_ERROR_INVALID_BLOCK = -8,
    CAFE_ERROR_GPU_NOT_AVAILABLE = -9,
    CAFE_ERROR_CUDA_ERROR = -10,
};
```

### Apêndice C: Tamanhos e Limites

```c
#define CAFE_MAX_DIMENSIONS      65535   // Máximo width/height
#define CAFE_MAX_CHANNELS        16      // Máximo de canais
#define CAFE_MAX_BIT_DEPTH       16      // Máximo bits/canal
#define CAFE_BLOCK_SIZE          128     // Tamanho padrão do bloco
#define CAFE_MAX_COMPRESSION_LVL 22      // ZSTD max level
#define CAFE_MAX_METADATA_SIZE   (16*1024*1024)  // 16MB
```

---

## Apêndice D: Extensões Planejadas (v2.0+)

As seguintes features estão planejadas para versões futuras do formato CAFE. Estas extensões não fazem parte do escopo v1.0, mas foram cuidadosamente consideradas e documentadas para futuras implementações.

### D.1 Video/Temporal Support 🎥

**Status**: Planejado para v2.0
**Prioridade**: ALTA
**Complexidade**: Média-Alta

Suporte nativo a sequências de vídeo e dados temporais para aplicações de video ML (action recognition, tracking, etc.).

```c
typedef struct {
    uint32_t sequence_id;        // ID da sequência
    uint32_t frame_number;       // Número do frame
    uint32_t total_frames;       // Total de frames
    float    frame_rate;         // FPS
    uint64_t timestamp_us;       // Timestamp em microsegundos

    // Compressão temporal
    uint8_t  is_keyframe;        // 1 se I-frame
    uint64_t reference_frame_id; // Frame de referência para P-frames

    // Optical flow pré-computado (opcional)
    uint64_t flow_data_offset;
} cafe_temporal_metadata_t;
```

**Benefícios**:
- Compressão inter-frame (delta encoding)
- Optical flow pré-computado
- Batch loading de sequências temporais
- Ideal para video action recognition, object tracking

**Casos de Uso**:
- Action recognition (Kinetics, UCF-101)
- Video segmentation
- Optical flow datasets
- Temporal activity detection

---

### D.2 Multi-Modal Data Support 🌈

**Status**: Planejado para v2.0
**Prioridade**: ALTA
**Complexidade**: Média

Suporte a múltiplas modalidades alinhadas (RGB + Depth + Normal + Segmentation).

```c
typedef enum {
    CAFE_MODALITY_RGB = 0,
    CAFE_MODALITY_DEPTH = 1,         // Depth map
    CAFE_MODALITY_NORMAL = 2,        // Normal map
    CAFE_MODALITY_SEMANTIC = 3,      // Semantic segmentation
    CAFE_MODALITY_INSTANCE = 4,      // Instance segmentation
    CAFE_MODALITY_FLOW = 5,          // Optical flow
    CAFE_MODALITY_ALBEDO = 6,        // Albedo (PBR)
    CAFE_MODALITY_METALLIC = 7,      // Metallic (PBR)
    CAFE_MODALITY_ROUGHNESS = 8,     // Roughness (PBR)
} cafe_modality_t;

typedef struct {
    uint64_t parent_image_id;    // Imagem RGB principal
    cafe_modality_t modality;    // Tipo de modalidade
    uint32_t width, height;      // Pode diferir da RGB
    cafe_pixel_format_t format;  // uint16 para depth, etc.
    uint64_t data_offset;
} cafe_modality_descriptor_t;
```

**Benefícios**:
- Um arquivo, múltiplas modalidades perfeitamente alinhadas
- Compressão especializada por modalidade
- Datasets RGB-D nativamente suportados (NYUv2, ScanNet, Matterport3D)

**Exemplo de Uso**:
```python
# Carregar RGB + Depth + Normal
with cafe.open("rgbd_dataset.cafe") as f:
    rgb = f.read_modality(img_id=0, modality='rgb')      # uint8 (H,W,3)
    depth = f.read_modality(img_id=0, modality='depth')  # uint16 (H,W,1)
    normal = f.read_modality(img_id=0, modality='normal')# float16 (H,W,3)
```

---

### D.3 Cloud-Native Optimizations ☁️

**Status**: Planejado para v2.0
**Prioridade**: ALTA
**Complexidade**: Média

Otimizações para object storage (S3, GCS, Azure Blob) com layout columnar e footer pattern.

```c
typedef struct {
    // Footer no final (como Parquet/ORC)
    uint64_t footer_offset;      // Permite ler metadata sem baixar tudo

    // Chunking otimizado para S3
    uint32_t optimal_chunk_size; // 5MB típico para S3

    // Bloom filter para queries rápidas
    uint8_t* bloom_filter;       // "Tem imagem com label X?"
    size_t   bloom_filter_size;

    // Layout columnar opcional
    uint8_t columnar_mode;       // Agrupar blocos por tipo
} cafe_cloud_layout_t;
```

**Benefícios**:
- Ler metadata sem baixar arquivo inteiro
- S3 Select-like queries
- Minimizar número de requests
- Footer pattern permite extensibilidade

**Exemplo**:
```python
# Query sem baixar dataset gigante
with cafe.open("s3://bucket/huge_dataset.cafe") as f:
    # Lê apenas footer (últimos KB)
    metadata = f.get_all_metadata()  # 1 request

    # Bloom filter: "Tem imagens de gatos?"
    if f.has_label('cat'):  # Sem iterar
        cat_images = f.query(label='cat')

    # Byte-range request apenas para blocos necessários
    img = f.read_image(cat_images[0])
```

---

### D.4 Smart Metadata Indexing 🔍

**Status**: Planejado para v2.0
**Prioridade**: ALTA
**Complexidade**: Média

Índices invertidos e espaciais para queries instantâneas em metadata.

```c
// Índice invertido: label -> [image_ids]
typedef struct {
    char key[64];                // "label"
    char value[64];              // "cat"
    uint32_t num_images;
    uint64_t* image_ids;         // Array de IDs
} cafe_inverted_index_entry_t;

// Índice espacial para bounding boxes (R-tree)
typedef struct {
    uint8_t tree_type;           // R_TREE, KD_TREE
    void* tree_data;             // Estrutura serializada
    size_t tree_size;
} cafe_spatial_index_t;
```

**Benefícios**:
```python
# Queries instantâneas (sem iterar 1M imagens)
cats = dataset.query(label='cat')              # Índice invertido
large_objects = dataset.query(bbox_area > 0.5) # Índice espacial
night_scenes = dataset.query(brightness < 50)  # Índice de features
```

---

### D.5 Region of Interest (ROI) Decoding 🎯

**Status**: Planejado para v2.1
**Prioridade**: MÉDIA
**Complexidade**: Baixa

Decodificar apenas região específica sem descomprimir imagem inteira.

```c
// Decodificar apenas ROI
int cafe_read_region(
    cafe_file_t* file,
    uint64_t image_id,
    int x, int y, int width, int height,  // ROI coordinates
    uint8_t** pixel_data
) {
    // 1. Determinar blocos que sobrepõem com ROI
    // 2. Descomprimir apenas esses blocos
    // 3. Extrair região exata
    // 4. Retornar apenas pixels da ROI
}
```

**Benefícios**:
- Data augmentation (random crops) extremamente eficiente
- Zoom/pan em viewers sem custo
- Ideal para imagens gigantes (gigapixel, satellite imagery)

---

### D.6 Multi-Resolution Pyramid 🏔️

**Status**: Planejado para v2.1
**Prioridade**: MÉDIA
**Complexidade**: Média

Mipmaps pré-computados para acesso multi-escala eficiente.

```c
typedef struct {
    uint8_t num_levels;          // 4-6 típico
    struct {
        uint32_t width, height;
        uint64_t data_offset;
        uint32_t compressed_size;
    } levels[MAX_PYRAMID_LEVELS];
} cafe_pyramid_t;

// Níveis típicos:
// Level 0: 100% (full resolution)
// Level 1: 50%
// Level 2: 25%
// Level 3: 12.5%
```

**Benefícios**:
- Multi-scale training (FPN, image pyramids)
- Progressive rendering rápido
- Thumbnail = nível mais baixo (grátis)
- Zoom out instantâneo

---

### D.7 Neural Codec Support 🧠

**Status**: Pesquisa / v3.0
**Prioridade**: INOVADORA (Research)
**Complexidade**: ALTA

Compressão neural de última geração (superando JPEG/WebP).

```c
typedef struct {
    char model_name[64];         // "ballé2018", "cheng2020-attn"
    uint32_t latent_dim;         // Dimensão do latent space
    uint8_t* latent_code;        // Código latente (comprimido)
    size_t latent_size;

    // Decoder (compartilhado ou inline)
    uint64_t decoder_offset;     // 0 se usar decoder externo
    char decoder_hash[32];       // SHA-256 do decoder
} cafe_neural_codec_t;
```

**Benefícios**:
- Compressão 50-100× superior a JPEG mantendo qualidade
- Latent space já é embedding (útil para ML!)
- Estado da arte, publicável em conferências

**Limitações**:
- Requer GPU para decodificação
- Decoder neural deve ser distribuído
- Ainda em pesquisa ativa

---

### D.8 Cryptographic Support 🔐

**Status**: Planejado para v2.2 (se demanda existir)
**Prioridade**: BAIXA (nicho)
**Complexidade**: Média

Encriptação e assinaturas digitais para datasets sensíveis.

```c
typedef struct {
    uint8_t encrypted;           // 1 se encriptado
    uint8_t encryption_algo;     // AES-256-GCM, ChaCha20
    uint8_t key_derivation;      // PBKDF2, Argon2
    uint8_t salt[32];
    uint8_t auth_tag[16];        // Authentication tag
} cafe_encryption_t;

typedef struct {
    uint8_t signed;              // 1 se assinado
    uint8_t signature_algo;      // Ed25519, RSA
    uint8_t public_key[32];
    uint8_t signature[64];
} cafe_signature_t;
```

**Casos de Uso**:
- Medical imaging (HIPAA compliance)
- GDPR-compliant datasets
- Verificação de autenticidade
- Datasets corporativos sensíveis

---

### D.9 Annotation Rendering 🎨

**Status**: Planejado para v2.2
**Prioridade**: BAIXA (nice-to-have)
**Complexidade**: Baixa

Renderização direta de annotations para visualização rápida.

```c
int cafe_render_annotations(
    cafe_file_t* file,
    uint64_t image_id,
    uint8_t** output_image,      // Imagem + annotations overlay
    cafe_render_options_t* options
);

typedef struct {
    uint8_t render_bboxes;
    uint8_t render_masks;
    uint8_t render_keypoints;
    uint32_t bbox_color;         // RGBA
    uint8_t bbox_thickness;
    float mask_alpha;            // Transparência
} cafe_render_options_t;
```

**Benefícios**:
- Visualização rápida sem código externo
- Útil para debugging e demos
- Reduz dependências em ferramentas

---

## Resumo de Extensões Planejadas

| Feature | Versão | Prioridade | Impacto | Complexidade |
|---------|--------|-----------|---------|--------------|
| Video/Temporal | v2.0 | 🔥🔥🔥 ALTA | Enorme (novo mercado) | ⚙️⚙️⚙️ |
| Multi-Modal | v2.0 | 🔥🔥🔥 ALTA | Muito Alto (RGB-D) | ⚙️⚙️ |
| Cloud-Native | v2.0 | 🔥🔥🔥 ALTA | Alto (cloud storage) | ⚙️⚙️ |
| Smart Indexing | v2.0 | 🔥🔥🔥 ALTA | Alto (queries) | ⚙️⚙️ |
| ROI Decoding | v2.1 | 🔥🔥 MÉDIA | Médio (efficiency) | ⚙️ |
| Multi-Res Pyramid | v2.1 | 🔥🔥 MÉDIA | Médio (multi-scale) | ⚙️⚙️ |
| Neural Codec | v3.0 | 🔥 RESEARCH | Inovador | ⚙️⚙️⚙️⚙️ |
| Encryption | v2.2 | 🔥 BAIXA | Baixo (nicho) | ⚙️⚙️ |
| Annotation Render | v2.2 | 🔥 BAIXA | Baixo (QoL) | ⚙️ |

**Nota**: Lossy compression (exceto neural codec) foi deliberadamente excluída do roadmap para manter foco em qualidade e lossless workflows.

---

**Documento de Especificação v1.0**
**Última atualização**: 22 de Fevereiro de 2026
**Autor**: Daniel Secco Ferreira e Silva
**Contato**: daniel.secco@computer.org

---

*Este documento descreve a especificação completa do formato CAFE v1.0, incluindo features core e extensões planejadas para versões futuras. Para status de implementação atual, consulte IMPLEMENTATION_PLAN.md*

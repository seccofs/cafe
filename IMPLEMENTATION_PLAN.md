# CAFE Format - Plano de Implementação em Fases

## Compression Adaptive Filtering Experiment

**Versão do Plano**: 1.0 (Starting from Zero)
**Data**: Fevereiro 2026
**Autor**: Daniel Secco
**Duração Estimada Total**: 24-30 meses (trabalho solo, part-time durante mestrado)

---

## Visão Geral

Este documento detalha o plano completo de implementação do formato CAFE **do zero até a versão 1.0 completa**, incluindo todos os recursos opcionais especificados. O projeto está iniciando agora, sem código implementado.

### Filosofia do Plano

1. **MVP First**: Implementar um Minimum Viable Product funcional rapidamente
2. **Iteração Incremental**: Cada fase adiciona features mantendo o que funciona
3. **Validação Contínua**: Testar e validar antes de avançar
4. **Flexibilidade**: Ajustar baseado em aprendizados e feedback
5. **Documentação Paralela**: Documentar decisões de design durante implementação

### Status Atual: ⚠️ Início do Projeto (Código Zero)

**Já Existe**:
- ✅ Conceito do formato
- ✅ Especificação técnica documentada
- ✅ Plano de implementação (este documento)

**Não Existe Ainda**:
- ❌ Nenhum código
- ❌ Nenhuma estrutura de projeto
- ❌ Nenhuma ferramenta
- ❌ Nenhum teste

---

## Fase 0: Bootstrap do Projeto (Mês 1)

**Objetivo**: Criar infraestrutura básica do projeto e ambiente de desenvolvimento

### 0.1 Estrutura do Projeto (Semana 1)

#### Tarefas:
- [ ] Criar repositório Git
  ```bash
  git init
  git remote add origin https://github.com/seccofs/cafe.git
  ```
- [ ] Estrutura de diretórios
  ```
  cafe/
  ├── .github/
  │   └── workflows/          # CI/CD (futuro)
  ├── docs/                   # Documentação
  │   ├── FORMAT_SPEC.md
  │   ├── API.md
  │   └── DESIGN_DECISIONS.md
  ├── include/                # Headers públicos
  │   └── cafe.h
  ├── src/                    # Código fonte
  │   ├── core/
  │   ├── compression/
  │   ├── io/
  │   └── util/
  ├── tests/                  # Testes
  │   ├── unit/
  │   └── integration/
  ├── tools/                  # Ferramentas CLI
  ├── examples/               # Exemplos de uso
  ├── benchmarks/             # Performance benchmarks
  ├── third_party/            # Dependências externas
  ├── CMakeLists.txt          # Build system
  ├── Makefile                # Build simplificado
  ├── README.md
  ├── LICENSE
  └── .gitignore
  ```
- [ ] Criar arquivos base (.gitignore, LICENSE, README inicial)
- [ ] Configurar .editorconfig para consistência de código

#### Entregáveis:
- Estrutura de diretórios completa
- README inicial com descrição do projeto
- LICENSE (MIT)
- .gitignore configurado para C/C++

#### Critérios de Sucesso:
- [ ] Estrutura organizada e lógica
- [ ] README explica propósito do projeto
- [ ] Repositório inicializado

### 0.2 Build System (Semana 2)

#### Tarefas:
- [ ] Criar CMakeLists.txt básico
  ```cmake
  cmake_minimum_required(VERSION 3.18)
  project(cafe VERSION 0.1.0 LANGUAGES C)

  set(CMAKE_C_STANDARD 11)
  set(CMAKE_C_STANDARD_REQUIRED ON)

  # Opções de build
  option(CAFE_BUILD_TESTS "Build tests" ON)
  option(CAFE_BUILD_TOOLS "Build command-line tools" ON)
  option(CAFE_BUILD_EXAMPLES "Build examples" ON)
  option(CAFE_ENABLE_ASAN "Enable Address Sanitizer" OFF)

  # Biblioteca principal
  add_library(cafe SHARED)
  add_library(cafe_static STATIC)

  # Instalação
  install(TARGETS cafe cafe_static
          LIBRARY DESTINATION lib
          ARCHIVE DESTINATION lib)
  install(DIRECTORY include/ DESTINATION include)
  ```
- [ ] Criar Makefile wrapper simples
  ```makefile
  .PHONY: all build test clean

  all: build

  build:
  	mkdir -p build && cd build && cmake .. && make

  test:
  	cd build && ctest --output-on-failure

  clean:
  	rm -rf build
  ```
- [ ] Configurar flags de compilação (warnings, otimizações)
- [ ] Testar build em Linux

#### Entregáveis:
- CMakeLists.txt funcional
- Makefile wrapper
- Build funcional (mesmo sem código ainda)

#### Critérios de Sucesso:
- [ ] `make build` executa sem erros
- [ ] Gera biblioteca vazia (placeholder)

### 0.3 Dependências Externas (Semanas 3-4)

#### Tarefas:
- [ ] **Zstandard (ZSTD)**
  ```cmake
  # CMakeLists.txt
  find_package(ZSTD REQUIRED)
  target_link_libraries(cafe PRIVATE zstd)
  ```
  - Opção 1: Link com biblioteca do sistema
  - Opção 2: Incluir como submodule (third_party/zstd)
  - Criar abstração para fácil substituição

- [ ] **FSE (Finite State Entropy)**
  ```bash
  git submodule add https://github.com/Cyan4973/FiniteStateEntropy third_party/fse
  ```
  - Adicionar como git submodule
  - Integrar no build system

- [ ] **Testes: Adicionar framework de testes**
  - Usar Check framework ou Unity
  - Ou criar framework minimal próprio

#### Configuração de Desenvolvimento:
- [ ] Documentar dependências em `docs/BUILDING.md`
- [ ] Script de setup para desenvolvedores
  ```bash
  # scripts/setup_dev.sh
  #!/bin/bash
  git submodule update --init --recursive
  # Instalar dependências do sistema
  # ...
  ```

#### Entregáveis:
- Dependências integradas no build
- `docs/BUILDING.md` com instruções
- Script de setup

#### Critérios de Sucesso:
- [ ] Build com todas as dependências
- [ ] Novo desenvolvedor consegue fazer build seguindo docs

### Marcos da Fase 0:

- **v0.1-dev**: Infraestrutura do projeto pronta
- **Data Alvo**: Fim da Semana 4
- **Critério de Sucesso**: Projeto compila, estrutura organizada, dependências integradas

---

## Fase 1: MVP - Formato Básico (Meses 2-4)

**Objetivo**: Implementar versão mínima funcional - salvar e carregar UMA imagem

### 1.1 Estruturas de Dados Fundamentais (Semanas 5-6)

#### Tarefas:
- [ ] Definir estruturas em `include/cafe.h`
  ```c
  // cafe.h - Versão inicial simplificada
  #ifndef CAFE_H
  #define CAFE_H

  #include <stdint.h>
  #include <stdio.h>

  // Magic number
  #define CAFE_MAGIC 0x45464143  // "CAFE"

  // Versão do formato
  #define CAFE_VERSION_MAJOR 0
  #define CAFE_VERSION_MINOR 1

  // Header do arquivo (versão simplificada - 64 bytes)
  typedef struct {
      uint32_t magic;              // "CAFE"
      uint16_t version_major;
      uint16_t version_minor;
      uint32_t width;              // Largura da imagem
      uint32_t height;             // Altura da imagem
      uint16_t channels;           // Canais (1, 3, 4)
      uint16_t bit_depth;          // Bits por canal (8)
      uint32_t num_blocks;         // Número de blocos
      uint32_t compressed_size;    // Tamanho comprimido total
      uint32_t uncompressed_size;  // Tamanho descomprimido
      uint32_t header_crc;         // CRC-32 do header
      uint8_t  reserved[24];       // Reservado
  } cafe_header_t;

  // Bloco de dados (128x128)
  typedef struct {
      uint16_t block_x;            // Índice X do bloco
      uint16_t block_y;            // Índice Y do bloco
      uint16_t width;              // Largura efetiva
      uint16_t height;             // Altura efetiva
      uint32_t compressed_size;
      uint32_t uncompressed_size;
      uint8_t* data;               // Dados comprimidos
  } cafe_block_t;

  // Handle do arquivo
  typedef struct cafe_file_t cafe_file_t;

  // API básica
  cafe_file_t* cafe_create(const char* path);
  cafe_file_t* cafe_open(const char* path);
  int cafe_close(cafe_file_t* file);

  int cafe_write_image(cafe_file_t* file, uint8_t* pixels,
                       int width, int height, int channels);
  int cafe_read_image(cafe_file_t* file, uint8_t** pixels,
                      int* width, int* height, int* channels);

  #endif // CAFE_H
  ```

- [ ] Implementar estrutura interna do arquivo
  ```c
  // src/core/cafe_internal.h
  struct cafe_file_t {
      FILE* fp;
      cafe_header_t header;
      int mode;  // 'r' ou 'w'
      cafe_block_t* blocks;
      int num_blocks;
  };
  ```

#### Entregáveis:
- `include/cafe.h` - API pública
- `src/core/cafe_internal.h` - Estruturas internas
- `src/core/cafe_types.c` - Funções auxiliares

#### Critérios de Sucesso:
- [ ] Estruturas bem documentadas
- [ ] Headers compilam sem erros
- [ ] Design revisado e aprovado

### 1.2 I/O Básico (Semanas 7-8)

#### Tarefas:
- [ ] Implementar abertura/fechamento de arquivos
  ```c
  // src/io/cafe_file.c
  cafe_file_t* cafe_create(const char* path) {
      cafe_file_t* file = calloc(1, sizeof(cafe_file_t));
      file->fp = fopen(path, "wb");
      if (!file->fp) {
          free(file);
          return NULL;
      }
      file->mode = 'w';
      return file;
  }

  cafe_file_t* cafe_open(const char* path) {
      cafe_file_t* file = calloc(1, sizeof(cafe_file_t));
      file->fp = fopen(path, "rb");
      if (!file->fp) {
          free(file);
          return NULL;
      }
      file->mode = 'r';

      // Ler header
      if (cafe_read_header(file) != 0) {
          cafe_close(file);
          return NULL;
      }
      return file;
  }

  int cafe_close(cafe_file_t* file) {
      if (!file) return -1;
      if (file->fp) fclose(file->fp);
      if (file->blocks) free(file->blocks);
      free(file);
      return 0;
  }
  ```

- [ ] Implementar leitura/escrita de header
  ```c
  // src/core/cafe_header.c
  int cafe_write_header(cafe_file_t* file) {
      cafe_header_t* h = &file->header;
      h->magic = CAFE_MAGIC;
      h->version_major = CAFE_VERSION_MAJOR;
      h->version_minor = CAFE_VERSION_MINOR;

      // Calcular CRC
      h->header_crc = cafe_crc32(h, sizeof(cafe_header_t) - 4);

      fwrite(h, sizeof(cafe_header_t), 1, file->fp);
      return 0;
  }

  int cafe_read_header(cafe_file_t* file) {
      cafe_header_t* h = &file->header;
      fread(h, sizeof(cafe_header_t), 1, file->fp);

      // Validar magic
      if (h->magic != CAFE_MAGIC) return -1;

      // Validar versão
      if (h->version_major > CAFE_VERSION_MAJOR) return -2;

      // Validar CRC
      uint32_t crc = cafe_crc32(h, sizeof(cafe_header_t) - 4);
      if (crc != h->header_crc) return -3;

      return 0;
  }
  ```

#### Entregáveis:
- `src/io/cafe_file.c`
- `src/core/cafe_header.c`

#### Critérios de Sucesso:
- [ ] Criar e abrir arquivos funciona
- [ ] Header é escrito e lido corretamente
- [ ] Validação de magic number e CRC

### 1.3 CRC-32 e Checksums (Semana 9)

#### Tarefas:
- [ ] Implementar CRC-32
  ```c
  // src/util/cafe_crc.c
  static uint32_t crc32_table[256];
  static int table_initialized = 0;

  void cafe_crc32_init(void) {
      for (uint32_t i = 0; i < 256; i++) {
          uint32_t crc = i;
          for (int j = 0; j < 8; j++) {
              crc = (crc >> 1) ^ ((crc & 1) ? 0xEDB88320 : 0);
          }
          crc32_table[i] = crc;
      }
      table_initialized = 1;
  }

  uint32_t cafe_crc32(const void* data, size_t size) {
      if (!table_initialized) cafe_crc32_init();

      const uint8_t* bytes = data;
      uint32_t crc = 0xFFFFFFFF;

      for (size_t i = 0; i < size; i++) {
          crc = crc32_table[(crc ^ bytes[i]) & 0xFF] ^ (crc >> 8);
      }

      return ~crc;
  }
  ```

- [ ] Testes unitários para CRC
  ```c
  // tests/unit/test_crc.c
  void test_crc32_known_values(void) {
      const char* test = "123456789";
      uint32_t crc = cafe_crc32(test, 9);
      assert(crc == 0xCBF43926);  // Valor conhecido
  }
  ```

#### Entregáveis:
- `src/util/cafe_crc.c`
- `tests/unit/test_crc.c`

#### Critérios de Sucesso:
- [ ] CRC implementado corretamente
- [ ] Testes passam com valores conhecidos

### 1.4 Divisão em Blocos (Semanas 10-11)

#### Tarefas:
- [ ] Implementar divisão da imagem em blocos 128×128
  ```c
  // src/core/cafe_block.c
  int cafe_create_blocks(cafe_file_t* file, uint8_t* pixels,
                         int width, int height, int channels) {
      const int BLOCK_SIZE = 128;

      int blocks_x = (width + BLOCK_SIZE - 1) / BLOCK_SIZE;
      int blocks_y = (height + BLOCK_SIZE - 1) / BLOCK_SIZE;
      int num_blocks = blocks_x * blocks_y;

      file->blocks = calloc(num_blocks, sizeof(cafe_block_t));
      file->num_blocks = num_blocks;

      int block_idx = 0;
      for (int by = 0; by < blocks_y; by++) {
          for (int bx = 0; bx < blocks_x; bx++) {
              cafe_block_t* block = &file->blocks[block_idx++];
              block->block_x = bx;
              block->block_y = by;

              // Calcular dimensões efetivas
              int x_start = bx * BLOCK_SIZE;
              int y_start = by * BLOCK_SIZE;
              block->width = min(BLOCK_SIZE, width - x_start);
              block->height = min(BLOCK_SIZE, height - y_start);

              // Extrair pixels do bloco
              int block_pixels_size = block->width * block->height * channels;
              block->data = malloc(block_pixels_size);

              // Copiar pixels
              for (int y = 0; y < block->height; y++) {
                  for (int x = 0; x < block->width; x++) {
                      int src_offset = ((y_start + y) * width + (x_start + x)) * channels;
                      int dst_offset = (y * block->width + x) * channels;
                      memcpy(&block->data[dst_offset],
                             &pixels[src_offset],
                             channels);
                  }
              }

              block->uncompressed_size = block_pixels_size;
          }
      }

      return 0;
  }
  ```

- [ ] Implementar reconstrução da imagem a partir de blocos
  ```c
  int cafe_reconstruct_image(cafe_file_t* file, uint8_t** pixels) {
      int width = file->header.width;
      int height = file->header.height;
      int channels = file->header.channels;

      *pixels = malloc(width * height * channels);

      for (int i = 0; i < file->num_blocks; i++) {
          cafe_block_t* block = &file->blocks[i];
          int x_start = block->block_x * 128;
          int y_start = block->block_y * 128;

          // Copiar pixels do bloco para imagem
          for (int y = 0; y < block->height; y++) {
              for (int x = 0; x < block->width; x++) {
                  int src_offset = (y * block->width + x) * channels;
                  int dst_offset = ((y_start + y) * width + (x_start + x)) * channels;
                  memcpy(&(*pixels)[dst_offset],
                         &block->data[src_offset],
                         channels);
              }
          }
      }

      return 0;
  }
  ```

#### Entregáveis:
- `src/core/cafe_block.c`
- `tests/unit/test_block.c`

#### Critérios de Sucesso:
- [ ] Divisão em blocos funciona para várias resoluções
- [ ] Reconstrução é bit-exact (sem perdas)
- [ ] Blocos nas bordas tratados corretamente

### 1.5 Compressão ZSTD (Semanas 12-13)

#### Tarefas:
- [ ] Integrar ZSTD para compressão de blocos
  ```c
  // src/compression/cafe_zstd.c
  #include <zstd.h>

  int cafe_compress_block(cafe_block_t* block, int compression_level) {
      size_t max_compressed = ZSTD_compressBound(block->uncompressed_size);
      uint8_t* compressed = malloc(max_compressed);

      size_t compressed_size = ZSTD_compress(
          compressed, max_compressed,
          block->data, block->uncompressed_size,
          compression_level
      );

      if (ZSTD_isError(compressed_size)) {
          free(compressed);
          return -1;
      }

      // Substituir dados não comprimidos por comprimidos
      free(block->data);
      block->data = realloc(compressed, compressed_size);
      block->compressed_size = compressed_size;

      return 0;
  }

  int cafe_decompress_block(cafe_block_t* block) {
      uint8_t* decompressed = malloc(block->uncompressed_size);

      size_t result = ZSTD_decompress(
          decompressed, block->uncompressed_size,
          block->data, block->compressed_size
      );

      if (ZSTD_isError(result)) {
          free(decompressed);
          return -1;
      }

      free(block->data);
      block->data = decompressed;

      return 0;
  }
  ```

- [ ] Testar diferentes níveis de compressão
- [ ] Medir razão de compressão

#### Entregáveis:
- `src/compression/cafe_zstd.c`
- `tests/unit/test_compression.c`
- `benchmarks/compression_ratio.c`

#### Critérios de Sucesso:
- [ ] Compressão/descompressão funciona
- [ ] Dados recuperados são idênticos ao original
- [ ] Razão de compressão >2:1 em média

### 1.6 API Completa de Escrita/Leitura (Semanas 14-16)

#### Tarefas:
- [ ] Implementar `cafe_write_image()`
  ```c
  // src/core/cafe_io.c
  int cafe_write_image(cafe_file_t* file, uint8_t* pixels,
                       int width, int height, int channels) {
      // Configurar header
      file->header.width = width;
      file->header.height = height;
      file->header.channels = channels;
      file->header.bit_depth = 8;

      // Criar blocos
      cafe_create_blocks(file, pixels, width, height, channels);

      // Comprimir cada bloco
      for (int i = 0; i < file->num_blocks; i++) {
          cafe_compress_block(&file->blocks[i], 3);  // Nível 3
      }

      // Atualizar header com tamanhos
      uint32_t total_compressed = 0;
      for (int i = 0; i < file->num_blocks; i++) {
          total_compressed += file->blocks[i].compressed_size;
      }
      file->header.compressed_size = total_compressed;
      file->header.uncompressed_size = width * height * channels;
      file->header.num_blocks = file->num_blocks;

      // Escrever header
      cafe_write_header(file);

      // Escrever blocos
      for (int i = 0; i < file->num_blocks; i++) {
          cafe_block_t* b = &file->blocks[i];

          // Escrever metadata do bloco
          fwrite(&b->block_x, sizeof(uint16_t), 1, file->fp);
          fwrite(&b->block_y, sizeof(uint16_t), 1, file->fp);
          fwrite(&b->width, sizeof(uint16_t), 1, file->fp);
          fwrite(&b->height, sizeof(uint16_t), 1, file->fp);
          fwrite(&b->compressed_size, sizeof(uint32_t), 1, file->fp);
          fwrite(&b->uncompressed_size, sizeof(uint32_t), 1, file->fp);

          // Escrever dados comprimidos
          fwrite(b->data, 1, b->compressed_size, file->fp);
      }

      return 0;
  }
  ```

- [ ] Implementar `cafe_read_image()`
  ```c
  int cafe_read_image(cafe_file_t* file, uint8_t** pixels,
                      int* width, int* height, int* channels) {
      *width = file->header.width;
      *height = file->header.height;
      *channels = file->header.channels;

      // Ler blocos
      file->blocks = calloc(file->header.num_blocks, sizeof(cafe_block_t));
      file->num_blocks = file->header.num_blocks;

      for (int i = 0; i < file->num_blocks; i++) {
          cafe_block_t* b = &file->blocks[i];

          // Ler metadata do bloco
          fread(&b->block_x, sizeof(uint16_t), 1, file->fp);
          fread(&b->block_y, sizeof(uint16_t), 1, file->fp);
          fread(&b->width, sizeof(uint16_t), 1, file->fp);
          fread(&b->height, sizeof(uint16_t), 1, file->fp);
          fread(&b->compressed_size, sizeof(uint32_t), 1, file->fp);
          fread(&b->uncompressed_size, sizeof(uint32_t), 1, file->fp);

          // Ler dados comprimidos
          b->data = malloc(b->compressed_size);
          fread(b->data, 1, b->compressed_size, file->fp);

          // Descomprimir
          cafe_decompress_block(b);
      }

      // Reconstruir imagem
      cafe_reconstruct_image(file, pixels);

      return 0;
  }
  ```

- [ ] Testes end-to-end
  ```c
  // tests/integration/test_roundtrip.c
  void test_write_read_roundtrip(void) {
      // Criar imagem de teste
      int width = 512, height = 512, channels = 3;
      uint8_t* original = create_test_image(width, height, channels);

      // Escrever
      cafe_file_t* out = cafe_create("test.cafe");
      cafe_write_image(out, original, width, height, channels);
      cafe_close(out);

      // Ler
      cafe_file_t* in = cafe_open("test.cafe");
      uint8_t* loaded;
      int w, h, c;
      cafe_read_image(in, &loaded, &w, &h, &c);
      cafe_close(in);

      // Verificar
      assert(w == width);
      assert(h == height);
      assert(c == channels);
      assert(memcmp(original, loaded, w*h*c) == 0);

      free(original);
      free(loaded);
  }
  ```

#### Entregáveis:
- `src/core/cafe_io.c` completo
- `tests/integration/test_roundtrip.c`

#### Critérios de Sucesso:
- [ ] Escrever e ler imagem funciona
- [ ] Roundtrip é lossless
- [ ] Funciona com várias resoluções

### Marcos da Fase 1:

- **v0.1-alpha**: MVP funcional - salvar/carregar uma imagem
- **Data Alvo**: Fim do Mês 4
- **Critério de Sucesso**: Converter PNG→CAFE→PNG sem perdas

---

## Fase 2: Primeira Ferramenta CLI (Mês 5)

**Objetivo**: Criar ferramenta de linha de comando para converter imagens

### 2.1 cafe-convert (Semanas 17-20)

#### Tarefas:
- [ ] Implementar ferramenta básica
  ```c
  // tools/cafe-convert.c
  #include <cafe.h>
  #include "stb_image.h"
  #include "stb_image_write.h"

  void print_usage(void) {
      printf("Usage:\n");
      printf("  cafe-convert --to-cafe input.png output.cafe\n");
      printf("  cafe-convert --from-cafe input.cafe output.png\n");
  }

  int main(int argc, char** argv) {
      if (argc < 4) {
          print_usage();
          return 1;
      }

      if (strcmp(argv[1], "--to-cafe") == 0) {
          // PNG → CAFE
          int w, h, c;
          uint8_t* pixels = stbi_load(argv[2], &w, &h, &c, 0);

          cafe_file_t* file = cafe_create(argv[3]);
          cafe_write_image(file, pixels, w, h, c);
          cafe_close(file);

          stbi_image_free(pixels);
          printf("Converted %s → %s\n", argv[2], argv[3]);
      }
      else if (strcmp(argv[1], "--from-cafe") == 0) {
          // CAFE → PNG
          cafe_file_t* file = cafe_open(argv[2]);
          uint8_t* pixels;
          int w, h, c;
          cafe_read_image(file, &pixels, &w, &h, &c);
          cafe_close(file);

          stbi_write_png(argv[3], w, h, c, pixels, w * c);

          free(pixels);
          printf("Converted %s → %s\n", argv[2], argv[3]);
      }
      else {
          print_usage();
          return 1;
      }

      return 0;
  }
  ```

- [ ] Adicionar stb_image para ler/escrever PNG
  ```bash
  # Adicionar stb_image.h ao projeto
  curl -o include/stb_image.h https://raw.githubusercontent.com/nothings/stb/master/stb_image.h
  curl -o include/stb_image_write.h https://raw.githubusercontent.com/nothings/stb/master/stb_image_write.h
  ```

- [ ] Integrar no build system
- [ ] Testar conversões

#### Entregáveis:
- `tools/cafe-convert.c`
- Executável `cafe-convert`

#### Critérios de Sucesso:
- [ ] Converte PNG→CAFE
- [ ] Converte CAFE→PNG
- [ ] Roundtrip é lossless

### Marcos da Fase 2:

- **v0.2-alpha**: Primeira ferramenta funcional
- **Data Alvo**: Fim do Mês 5
- **Demonstração**: Converter imagem real PNG→CAFE→PNG

---

## Fase 3: Multi-Imagem Container (Meses 6-7)

**Objetivo**: Suportar múltiplas imagens em um único arquivo

### 3.1 Redesign do Formato (Semanas 21-22)

#### Tarefas:
- [ ] Atualizar estruturas de dados
  ```c
  // Nova estrutura de header (256 bytes)
  typedef struct {
      uint8_t  magic[4];           // "CAFE"
      uint16_t version_major;
      uint16_t version_minor;
      uint64_t total_images;       // NOVO
      uint64_t total_blocks;
      uint64_t descriptor_offset;  // NOVO
      uint64_t index_offset;       // NOVO
      uint64_t data_offset;        // NOVO
      // ... resto do header de 256 bytes
  } cafe_file_header_v2_t;

  // Descriptor de imagem (160 bytes)
  typedef struct {
      uint64_t image_id;
      char     filename[64];
      uint32_t width;
      uint32_t height;
      uint16_t num_blocks_x;
      uint16_t num_blocks_y;
      uint16_t num_channels;
      uint16_t bit_depth;
      uint32_t compressed_size;
      uint32_t uncompressed_size;
      uint64_t data_offset;
      uint32_t header_crc;
      uint8_t  reserved[52];
  } cafe_image_descriptor_t;
  ```

- [ ] Layout do arquivo multi-imagem
  ```
  [File Header - 256 bytes]
  [Image Descriptors Array]
    - Descriptor 0
    - Descriptor 1
    - ...
  [Block Index Table]
  [Image 0 Blocks]
  [Image 1 Blocks]
  ...
  ```

#### Entregáveis:
- Estruturas atualizadas
- Documentação do novo layout

### 3.2 Implementação Multi-Imagem (Semanas 23-26)

#### Tarefas:
- [ ] API para adicionar múltiplas imagens
  ```c
  // Nova API
  cafe_file_t* cafe_create(const char* path);
  int cafe_add_image(cafe_file_t* file, uint8_t* pixels,
                     int width, int height, int channels,
                     const char* name);
  int cafe_finalize(cafe_file_t* file);  // Finaliza escrita

  cafe_file_t* cafe_open(const char* path);
  int cafe_get_num_images(cafe_file_t* file);
  int cafe_read_image_by_id(cafe_file_t* file, uint64_t id,
                            uint8_t** pixels, ...);
  int cafe_read_image_by_index(cafe_file_t* file, int index,
                               uint8_t** pixels, ...);
  ```

- [ ] Implementar escrita incremental
- [ ] Implementar índice de imagens
- [ ] Suporte a leitura aleatória

#### Entregáveis:
- API multi-imagem completa
- Testes com 100, 1000, 10000 imagens

#### Critérios de Sucesso:
- [ ] Container com 10K imagens funciona
- [ ] Leitura aleatória eficiente
- [ ] Overhead <2%

### Marcos da Fase 3:

- **v0.3-alpha**: Multi-imagem funcional
- **Data Alvo**: Fim do Mês 7
- **Demonstração**: Dataset com 10K imagens

---

## Fase 4: Ferramentas e Benchmarks (Meses 8-9)

**Objetivo**: Ferramentas CLI e primeiros benchmarks

### 4.1 cafe-inspect (Semanas 27-28)

```c
// tools/cafe-inspect.c
// Mostrar informações do arquivo
void inspect_file(const char* path) {
    cafe_file_t* file = cafe_open(path);

    printf("CAFE Format v%d.%d\n",
           file->header.version_major,
           file->header.version_minor);
    printf("Total images: %lu\n", file->header.total_images);
    printf("Total blocks: %lu\n", file->header.total_blocks);
    printf("File size: %lu bytes\n", get_file_size(path));

    // Estatísticas de compressão
    double ratio = (double)file->header.uncompressed_size /
                   file->header.compressed_size;
    printf("Compression ratio: %.2f:1\n", ratio);

    cafe_close(file);
}
```

### 4.2 cafe-bench (Semanas 29-30)

```c
// benchmarks/cafe-bench.c
// Benchmark de loading
void benchmark_sequential_load(const char* cafe_path) {
    cafe_file_t* file = cafe_open(cafe_path);
    int num_images = cafe_get_num_images(file);

    double start = get_time();

    for (int i = 0; i < num_images; i++) {
        uint8_t* pixels;
        int w, h, c;
        cafe_read_image_by_index(file, i, &pixels, &w, &h, &c);
        free(pixels);
    }

    double end = get_time();
    double elapsed = end - start;

    printf("Loaded %d images in %.2f seconds\n", num_images, elapsed);
    printf("Throughput: %.2f images/sec\n", num_images / elapsed);

    cafe_close(file);
}
```

### 4.3 Benchmark vs PNG (Semanas 31-32)

- [ ] Criar dataset de teste (1000 imagens PNG)
- [ ] Converter para CAFE
- [ ] Medir:
  - Tamanho em disco
  - Tempo de loading sequencial
  - Tempo de loading aleatório
- [ ] Gerar relatório

#### Entregáveis:
- `tools/cafe-inspect`
- `benchmarks/cafe-bench`
- Relatório de benchmark inicial

#### Critérios de Sucesso:
- [ ] Ferramentas funcionais
- [ ] Benchmark mostra viabilidade do formato
- [ ] Identificar áreas de otimização

### Marcos da Fase 4:

- **v0.4-alpha**: Ferramentas e benchmarks
- **Data Alvo**: Fim do Mês 9
- **Demonstração**: Benchmark CAFE vs PNG

---

## Fase 5: Otimizações Básicas (Meses 10-11)

**Objetivo**: Otimizar performance para uso real

### 5.1 Predictores (Semanas 33-36)

- [ ] Implementar differential predictor
- [ ] Implementar Paeth predictor (opcional)
- [ ] Medir impacto na compressão

### 5.2 Memory-Mapped I/O (Semanas 37-38)

- [ ] Implementar suporte a mmap
- [ ] Benchmark vs read() tradicional

### 5.3 Suporte HDR (High Dynamic Range) (Semanas 39-44)

**Objetivo**: Adicionar suporte completo a imagens HDR com formatos floating-point

#### 5.3.1 Tipos de Pixel Float (Semanas 39-40)

**Tarefas**:
- [ ] Definir enums e tipos
  ```c
  // include/cafe_hdr.h
  typedef enum {
      CAFE_PIXEL_UINT8 = 0,
      CAFE_PIXEL_UINT10 = 1,
      CAFE_PIXEL_UINT12 = 2,
      CAFE_PIXEL_UINT16 = 3,
      CAFE_PIXEL_FLOAT16 = 4,  // NOVO: Half-precision
      CAFE_PIXEL_FLOAT32 = 5,  // NOVO: Single-precision
  } cafe_pixel_format_t;

  typedef enum {
      CAFE_COLORSPACE_SRGB = 0,
      CAFE_COLORSPACE_LINEAR = 1,
      CAFE_COLORSPACE_REC709 = 2,
      CAFE_COLORSPACE_REC2020 = 3,
      CAFE_COLORSPACE_DCIP3 = 4,
      CAFE_COLORSPACE_ACESCG = 5,
  } cafe_colorspace_t;
  ```

- [ ] Implementar conversão float16 ↔ float32
  ```c
  // src/util/cafe_float16.c
  typedef uint16_t float16_t;

  float16_t float_to_float16(float value) {
      // IEEE 754 half-precision conversion
      uint32_t f32 = *(uint32_t*)&value;
      uint16_t sign = (f32 >> 16) & 0x8000;
      uint32_t exponent = (f32 >> 23) & 0xFF;
      uint32_t mantissa = f32 & 0x7FFFFF;

      // Conversão (tratando casos especiais)
      // ...
      return (float16_t)(sign | exp16 | mant16);
  }

  float float16_to_float(float16_t value) {
      // Conversão reversa
      // ...
  }
  ```

- [ ] Atualizar Image Descriptor
  ```c
  typedef struct {
      // ... campos existentes ...

      // NOVOS campos HDR
      uint8_t  pixel_format;       // cafe_pixel_format_t
      uint8_t  colorspace;         // cafe_colorspace_t
      uint8_t  transfer_function;  // Linear, sRGB, PQ, HLG
      uint8_t  is_hdr;             // Flag booleano
      float    white_point;        // Nits (e.g., 100.0, 10000.0)
      float    black_point;        // Nits (normalmente 0.0)
  } cafe_image_descriptor_t;
  ```

**Entregáveis**:
- `include/cafe_hdr.h` - Definições HDR
- `src/util/cafe_float16.c` - Conversões float16
- Estruturas atualizadas

**Critérios de Sucesso**:
- [ ] Conversão float16 ↔ float32 bit-exact
- [ ] Testes com valores conhecidos passam
- [ ] Estruturas compilam

#### 5.3.2 I/O de Imagens HDR (Semanas 41-42)

**Tarefas**:
- [ ] Implementar leitura/escrita float16
  ```c
  // src/core/cafe_io_hdr.c
  int cafe_add_image_hdr16(
      cafe_file_t* file,
      float16_t* pixel_data,
      int width, int height, int channels,
      cafe_hdr_params_t* params
  ) {
      // Criar blocos
      cafe_create_blocks_hdr16(file, pixel_data, width, height, channels);

      // Comprimir (ZSTD funciona com qualquer dado binário)
      for (int i = 0; i < file->num_blocks; i++) {
          cafe_compress_block(&file->blocks[i], params->compression_level);
      }

      // Atualizar descriptor com info HDR
      file->descriptor.pixel_format = CAFE_PIXEL_FLOAT16;
      file->descriptor.colorspace = params->colorspace;
      file->descriptor.white_point = params->white_point;
      file->descriptor.is_hdr = 1;

      return cafe_write_blocks(file);
  }
  ```

- [ ] Implementar leitura/escrita float32
  ```c
  int cafe_add_image_hdr32(
      cafe_file_t* file,
      float* pixel_data,
      int width, int height, int channels,
      cafe_hdr_params_t* params
  ) {
      // Similar a float16, mas com float32
      // ...
  }

  int cafe_read_image_hdr(
      cafe_file_t* file,
      uint64_t image_id,
      void** pixel_data,
      cafe_pixel_format_t* format
  ) {
      // Ler descriptor para saber o formato
      cafe_image_descriptor_t desc;
      cafe_read_image_descriptor(file, image_id, &desc);

      *format = desc.pixel_format;

      if (desc.pixel_format == CAFE_PIXEL_FLOAT16) {
          float16_t** pixels = (float16_t**)pixel_data;
          return cafe_read_image_float16(file, image_id, pixels);
      } else if (desc.pixel_format == CAFE_PIXEL_FLOAT32) {
          float** pixels = (float**)pixel_data;
          return cafe_read_image_float32(file, image_id, pixels);
      }
      // ... outros formatos
  }
  ```

- [ ] Testes roundtrip HDR
  ```c
  // tests/integration/test_hdr_roundtrip.c
  void test_float16_roundtrip() {
      // Criar imagem HDR teste
      int width = 512, height = 512, channels = 3;
      float16_t* hdr_pixels = create_hdr_test_image_f16(width, height, channels);

      // Escrever
      cafe_file_t* out = cafe_create("test_hdr.cafe");
      cafe_hdr_params_t params = {
          .colorspace = CAFE_COLORSPACE_LINEAR,
          .white_point = 100.0f,
      };
      cafe_add_image_hdr16(out, hdr_pixels, width, height, channels, &params);
      cafe_close(out);

      // Ler
      cafe_file_t* in = cafe_open("test_hdr.cafe");
      float16_t* loaded_pixels;
      cafe_pixel_format_t format;
      cafe_read_image_hdr(in, 0, (void**)&loaded_pixels, &format);

      // Verificar
      assert(format == CAFE_PIXEL_FLOAT16);
      assert(memcmp(hdr_pixels, loaded_pixels,
                    width * height * channels * sizeof(float16_t)) == 0);

      free(hdr_pixels);
      free(loaded_pixels);
      cafe_close(in);
  }
  ```

**Entregáveis**:
- `src/core/cafe_io_hdr.c`
- `tests/integration/test_hdr_roundtrip.c`

**Critérios de Sucesso**:
- [ ] Roundtrip float16 lossless
- [ ] Roundtrip float32 lossless
- [ ] Compressão ZSTD funciona (~2:1 para float16, ~1.5:1 para float32)

#### 5.3.3 Tone Mapping e Conversões (Semanas 43-44)

**Tarefas**:
- [ ] Implementar tone mapping operators
  ```c
  // src/hdr/cafe_tonemap.c

  // Reinhard global
  void cafe_tonemap_reinhard(
      float* hdr_pixels,
      uint8_t* ldr_pixels,
      int width, int height, int channels,
      float white_point
  ) {
      for (int i = 0; i < width * height * channels; i++) {
          float hdr = hdr_pixels[i];
          float l = hdr / white_point;

          // Reinhard operator
          float mapped = l / (1.0f + l);

          // Gamma correction (sRGB)
          mapped = powf(mapped, 1.0f/2.2f);

          ldr_pixels[i] = (uint8_t)(fminf(mapped * 255.0f, 255.0f));
      }
  }

  // ACES filmic
  void cafe_tonemap_aces(
      float* hdr_pixels,
      uint8_t* ldr_pixels,
      int width, int height, int channels,
      float exposure
  ) {
      // ACES fitted curve
      const float a = 2.51f;
      const float b = 0.03f;
      const float c = 2.43f;
      const float d = 0.59f;
      const float e = 0.14f;

      for (int i = 0; i < width * height * channels; i++) {
          float x = hdr_pixels[i] * exposure;
          float mapped = (x * (a*x + b)) / (x * (c*x + d) + e);
          mapped = fminf(fmaxf(mapped, 0.0f), 1.0f);
          ldr_pixels[i] = (uint8_t)(mapped * 255.0f);
      }
  }
  ```

- [ ] API de conversão conveniente
  ```c
  // Ler HDR e converter para LDR automaticamente
  int cafe_read_hdr_as_ldr(
      cafe_file_t* file,
      uint64_t image_id,
      uint8_t** ldr_pixels,
      cafe_tonemap_params_t* tonemap_params
  ) {
      // Ler como HDR
      float* hdr_pixels;
      cafe_read_image_hdr32(file, image_id, &hdr_pixels);

      // Alocar LDR
      int size = desc.width * desc.height * desc.channels;
      *ldr_pixels = malloc(size);

      // Tone mapping
      switch (tonemap_params->operator) {
          case CAFE_TONEMAP_REINHARD:
              cafe_tonemap_reinhard(hdr_pixels, *ldr_pixels, ...);
              break;
          case CAFE_TONEMAP_ACES:
              cafe_tonemap_aces(hdr_pixels, *ldr_pixels, ...);
              break;
      }

      free(hdr_pixels);
      return 0;
  }
  ```

- [ ] Implementar inverse tone mapping (básico)
  ```c
  // src/hdr/cafe_inverse_tonemap.c
  void cafe_inverse_tonemap_simple(
      uint8_t* ldr_pixels,
      float* hdr_pixels,
      int width, int height, int channels,
      float white_point
  ) {
      for (int i = 0; i < width * height * channels; i++) {
          float srgb = ldr_pixels[i] / 255.0f;

          // Remover gamma
          float linear = powf(srgb, 2.2f);

          // Inverse Reinhard (aproximado)
          float hdr = (linear / (1.0f - linear)) * white_point;

          hdr_pixels[i] = hdr;
      }
  }
  ```

- [ ] Testes de conversão
  ```c
  // Teste: HDR → tonemap → visual inspection
  void test_tonemap_visual() {
      // Carregar HDR
      float* hdr = load_test_hdr("memorial.exr");

      // Tone map
      uint8_t* ldr = malloc(width * height * 3);
      cafe_tonemap_aces(hdr, ldr, width, height, 3, 1.0f);

      // Salvar para inspeção visual
      save_png("tonemap_result.png", ldr, width, height, 3);
  }
  ```

**Entregáveis**:
- `src/hdr/cafe_tonemap.c`
- `src/hdr/cafe_inverse_tonemap.c`
- `tests/visual/test_tonemap_visual.c`

**Critérios de Sucesso**:
- [ ] Tone mapping produz imagens visualmente aceitáveis
- [ ] Reinhard e ACES implementados
- [ ] API de conversão funciona

#### 5.3.4 Ferramentas CLI para HDR (Semana 44)

**Tarefas**:
- [ ] Atualizar cafe-convert para HDR
  ```bash
  # OpenEXR → CAFE
  cafe-convert --to-cafe input.exr output.cafe \
    --pixel-format float32 \
    --colorspace linear \
    --white-point 100.0

  # CAFE HDR → PNG (com tone mapping)
  cafe-convert --from-cafe input.cafe output.png \
    --tonemap aces \
    --exposure 1.5

  # CAFE HDR → OpenEXR
  cafe-convert --from-cafe input.cafe output.exr
  ```

- [ ] Atualizar cafe-inspect para mostrar info HDR
  ```bash
  $ cafe-inspect scene.cafe

  CAFE Format v1.0
  ================
  Total images: 1

  Image 0:
    Resolution: 1920×1080
    Channels: 3 (RGB)
    Pixel Format: FLOAT32       # NOVO
    Colorspace: Linear RGB       # NOVO
    HDR: Yes                     # NOVO
    White Point: 100.0 nits      # NOVO
    Max Luminance: 10000.0 nits  # NOVO
    Compressed: 45.2 MB
    Uncompressed: 23.7 MB
    Ratio: 1.91:1
  ```

**Entregáveis**:
- `tools/cafe-convert.c` atualizado
- `tools/cafe-inspect.c` atualizado
- Integração com OpenEXR (via biblioteca)

**Critérios de Sucesso**:
- [ ] Converter EXR ↔ CAFE funciona
- [ ] Tone mapping em linha de comando funciona
- [ ] Informações HDR exibidas corretamente

### Marcos da Fase 5:

- **v0.5-beta**: Otimizações básicas + Suporte HDR completo
- **Data Alvo**: Fim do Mês 11
- **Demonstração**: Converter OpenEXR → CAFE → tonemap para PNG

---

## Fase 6: Python Bindings (Meses 12-14)

**Objetivo**: Integração com ecossistema Python/ML

**Nota**: Os bindings Python devem incluir suporte completo a HDR implementado na Fase 5.

### 6.1 CFFI Bindings (Semanas 45-50)

- [ ] Criar wrapper Python
- [ ] API pythonica
- [ ] Integração NumPy
- [ ] **Suporte HDR**:
  - [ ] Wrapper para `cafe_add_image_hdr16()` e `cafe_add_image_hdr32()`
  - [ ] Retornar arrays NumPy com dtype correto (float16, float32)
  - [ ] API para tone mapping
  ```python
  import cafe

  # Adicionar HDR
  hdr_img = np.array(..., dtype=np.float32)  # Linear RGB
  with cafe.create("hdr.cafe") as f:
      f.add_image(hdr_img,
                  pixel_format=cafe.PIXEL_FLOAT32,
                  colorspace=cafe.COLORSPACE_LINEAR,
                  white_point=100.0)

  # Ler HDR
  with cafe.open("hdr.cafe") as f:
      hdr_img = f.read_image(0, as_hdr=True)  # dtype=float32
      ldr_img = f.read_image(0, tonemap='aces')  # dtype=uint8
  ```

### 6.2 PyTorch DataLoader (Semanas 51-56)

- [ ] Classe CAFEDataset
- [ ] **Suporte a datasets HDR**:
  - [ ] Opção para retornar HDR (float) ou LDR (uint8)
  - [ ] Tone mapping on-the-fly para treinamento
  - [ ] Data augmentation compatível com HDR
- [ ] Testes de treinamento (LDR e HDR)

### Marcos da Fase 6:

- **v0.6-beta**: Python integration
- **Data Alvo**: Fim do Mês 14
- **Demonstração**: Training loop em PyTorch

---

## Fase 7: GPU Acceleration (Meses 15-18)

**Objetivo**: Aceleração GPU com CUDA/nvCOMP

### 7.1 Setup CUDA (Semanas 53-56)

- [ ] Configurar ambiente
- [ ] Integrar nvCOMP
- [ ] POC de descompressão GPU

### 7.2 Pipeline GPU (Semanas 57-64)

- [ ] Batch decompression
- [ ] Pipeline otimizado
- [ ] Benchmark GPU vs CPU

### Marcos da Fase 7:

- **v0.7-beta**: GPU acceleration
- **Data Alvo**: Fim do Mês 18
- **Meta**: 10× speedup vs CPU

---

## Fase 8: AI Metadata (Meses 19-21)

**Objetivo**: Sistema de metadados para ML

### 8.1 Framework de Metadata (Semanas 65-72)

- [ ] Estrutura de chunks
- [ ] Embeddings
- [ ] Labels e annotations

### Marcos da Fase 8:

- **v0.8-beta**: AI metadata
- **Data Alvo**: Fim do Mês 21

---

## Fase 9: Features Avançadas (Meses 22-24)

**Objetivo**: Progressive decoding, streaming

### 9.1 Thumbnails e Progressive (Semanas 73-80)

- [ ] Geração de thumbnails
- [ ] Progressive decoding

### 9.2 HTTP Streaming (Semanas 81-88)

- [ ] Suporte a byte-range
- [ ] Web viewer

### Marcos da Fase 9:

- **v0.9-rc**: Features avançadas
- **Data Alvo**: Fim do Mês 24

---

## Fase 10: Release 1.0 (Meses 25-30)

**Objetivo**: Polimento, documentação, publicação

### 10.1 Documentação Completa (Meses 25-26)

- [ ] API reference
- [ ] User guide
- [ ] Research paper

### 10.2 Testes Extensivos (Meses 27-28)

- [ ] Test suite completo
- [ ] Fuzzing
- [ ] Security audit

### 10.3 Release e Publicação (Meses 29-30)

- [ ] Release 1.0
- [ ] Publicar paper
- [ ] Website e divulgação

### Marcos da Fase 10:

- **v1.0**: Release final
- **Data Alvo**: Fim do Mês 30

---

## Timeline Resumida

```
Fase 0: Bootstrap (M1)          ████
Fase 1: MVP (M2-4)              ████████████
Fase 2: CLI Tool (M5)           ████
Fase 3: Multi-Image (M6-7)      ████████
Fase 4: Tools/Bench (M8-9)      ████████
Fase 5: Otimização+HDR (M10-11) ████████         ⭐ HDR Support
Fase 6: Python (M12-14)         ████████████
Fase 7: GPU (M15-18)            ████████████████
Fase 8: AI Meta (M19-21)        ████████████
Fase 9: Avançado (M22-24)       ████████████
Fase 10: Release (M25-30)       ████████████████████████
```

---

## Próximos Passos Imediatos

### Semana 1 (Agora):
1. ✅ Criar repositório Git
2. ✅ Estrutura de diretórios
3. ✅ README inicial
4. ✅ LICENSE (MIT)

### Semana 2:
5. [ ] Setup CMake
6. [ ] Integrar ZSTD
7. [ ] Primeiro hello world que compila

### Semana 3-4:
8. [ ] Implementar estruturas de dados básicas
9. [ ] CRC-32
10. [ ] Primeiro teste que passa

---

## Recursos HDR - Resumo da Implementação

O suporte a HDR (High Dynamic Range) foi integrado ao plano na **Fase 5** e representa uma capacidade importante para aplicações modernas de ML/CV.

### O que HDR adiciona:

**Formatos de Pixel**:
- ✅ FLOAT16 (half-precision): 2 bytes/canal, ~65K range
- ✅ FLOAT32 (single-precision): 4 bytes/canal, range ilimitado

**Espaços de Cor**:
- Linear RGB, sRGB, Rec.709, Rec.2020, DCI-P3, ACEScg

**Transfer Functions**:
- Linear, PQ (HDR10), HLG (broadcast HDR)

**Operações**:
- Tone mapping (Reinhard, ACES, filmic)
- Inverse tone mapping (LDR → pseudo-HDR)
- Conversões automáticas

**Casos de Uso**:
- Renderização 3D e VFX
- Fotografia computacional
- Medical/scientific imaging
- ML para relighting, tone mapping, HDR synthesis

### Impacto no Projeto:

| Aspecto | Impacto |
|---------|---------|
| **Complexidade** | +20% (conversões float, tone mapping) |
| **Tamanho Arquivo** | 2-4× maior vs LDR (mitigado por compressão) |
| **Compressão** | Menos eficiente (~2:1 float16, ~1.5:1 float32) |
| **Performance** | Similar (ZSTD funciona bem) |
| **Valor Agregado** | Alto - abre casos de uso profissionais |

### Timeline HDR:
- **Semanas 39-44** (Fase 5): Implementação completa
- **Semanas 45-50** (Fase 6): Python bindings com HDR
- **Mês 11**: HDR funcional e validado

---

## Extensões Futuras (Post-v1.0)

As seguintes features foram identificadas como valiosas para versões futuras do CAFE. Estão documentadas aqui para referência, mas **NÃO fazem parte do escopo v1.0**.

### Fase 11: Advanced Features (Pós-Release v1.0) 🚀

**Quando**: Após conclusão e estabilização da v1.0 (Mês 31+)
**Duração**: 12-18 meses adicionais
**Status**: Standby - aguardando feedback da comunidade

---

#### 11.1 Video/Temporal Support 🎥

**Prioridade**: ALTA ⭐⭐⭐
**Duração**: 3-4 meses
**Impacto**: Abre mercado de video ML (action recognition, tracking, etc.)

**Features**:
- Sequências temporais com compressão inter-frame
- Keyframes (I-frames) e delta frames (P-frames)
- Optical flow pré-computado
- API para batch loading de sequências

**Estruturas Propostas**:
```c
typedef struct {
    uint32_t sequence_id;
    uint32_t frame_number;
    float    frame_rate;
    uint8_t  is_keyframe;
    uint64_t reference_frame_id;  // Para P-frames
} cafe_temporal_metadata_t;
```

**Casos de Uso**:
- Video action recognition (Kinetics, UCF-101)
- Optical flow datasets
- Temporal activity detection
- Video object tracking

**Complexidade**: Média-Alta (compressão temporal, sincronização)

---

#### 11.2 Multi-Modal Data Support 🌈

**Prioridade**: ALTA ⭐⭐⭐
**Duração**: 2-3 meses
**Impacto**: Datasets RGB-D nativamente suportados

**Features**:
- RGB + Depth + Normal maps em um arquivo
- Semantic/Instance segmentation como modalidades
- PBR maps (albedo, metallic, roughness)
- Compressão especializada por modalidade

**Estruturas Propostas**:
```c
typedef enum {
    CAFE_MODALITY_RGB,
    CAFE_MODALITY_DEPTH,
    CAFE_MODALITY_NORMAL,
    CAFE_MODALITY_SEMANTIC,
    // ... etc
} cafe_modality_t;
```

**Datasets Alvo**:
- NYUv2 (RGB-D)
- ScanNet (RGB-D + semantic)
- Matterport3D
- Datasets de rendering (PBR)

**Complexidade**: Média (alinhamento espacial, múltiplos formatos)

---

#### 11.3 Cloud-Native Optimizations ☁️

**Prioridade**: ALTA ⭐⭐⭐
**Duração**: 2-3 meses
**Impacto**: Otimização para S3/GCS/Azure Blob

**Features**:
- Footer pattern (metadata no final, como Parquet)
- Bloom filters para queries rápidas
- Chunking otimizado para S3 byte-range
- Layout columnar opcional

**Benefícios**:
```python
# Ler metadata sem baixar GB
with cafe.open("s3://bucket/huge.cafe") as f:
    metadata = f.get_all_metadata()  # 1 request (footer)

    # Bloom filter query
    if f.has_label('cat'):
        cat_images = f.query(label='cat')
```

**Complexidade**: Média (layout de arquivo, integração cloud SDKs)

---

#### 11.4 Smart Metadata Indexing 🔍

**Prioridade**: ALTA ⭐⭐⭐
**Duração**: 2-3 meses
**Impacto**: Queries instantâneas em datasets massivos

**Features**:
- Índice invertido (label → [image_ids])
- Índice espacial para bounding boxes (R-tree)
- Índice de features numéricas
- Query engine SQL-like

**Exemplo**:
```python
# Query sem iterar milhões de imagens
cats = dataset.query(label='cat')
large = dataset.query(bbox_area > 0.5)
bright = dataset.query(brightness > 200)
```

**Complexidade**: Média (estruturas de índice, query engine)

---

#### 11.5 Region of Interest (ROI) Decoding 🎯

**Prioridade**: MÉDIA ⭐⭐
**Duração**: 2 meses
**Impacto**: Efficiency para crops e zoom

**Features**:
- Decodificar apenas região específica
- Sem descomprimir imagem inteira
- Ideal para data augmentation (random crops)

**API**:
```c
int cafe_read_region(
    cafe_file_t* file,
    uint64_t image_id,
    int x, int y, int width, int height,
    uint8_t** pixel_data
);
```

**Casos de Uso**:
- Random crops em treinamento
- Zoom/pan em viewers
- Gigapixel imagery
- Satellite imagery

**Complexidade**: Baixa (determinar blocos, extrair região)

---

#### 11.6 Multi-Resolution Pyramid 🏔️

**Prioridade**: MÉDIA ⭐⭐
**Duração**: 2-3 meses
**Impacto**: Multi-scale ML, progressive rendering

**Features**:
- Mipmaps pré-computados (100%, 50%, 25%, ...)
- Acesso rápido a qualquer escala
- Progressive rendering grátis
- Thumbnail = nível mais baixo

**Estrutura**:
```c
typedef struct {
    uint8_t num_levels;  // 4-6 típico
    struct {
        uint32_t width, height;
        uint64_t data_offset;
    } levels[MAX_LEVELS];
} cafe_pyramid_t;
```

**Casos de Uso**:
- FPN (Feature Pyramid Networks)
- Multi-scale training
- Image pyramids para detecção
- Web viewers (progressive loading)

**Complexidade**: Média (geração de níveis, storage overhead)

---

#### 11.7 Neural Codec Support 🧠

**Prioridade**: RESEARCH / INOVADORA ⭐⭐⭐⭐
**Duração**: 4-6 meses
**Impacto**: Estado da arte em compressão (publicável!)

**Features**:
- Compressão neural (Ballé, Cheng, etc.)
- 50-100× compressão mantendo qualidade
- Latent space como embedding (grátis!)
- Decoder neural incluído ou externo

**Estrutura**:
```c
typedef struct {
    char model_name[64];      // "cheng2020-attn"
    uint32_t latent_dim;
    uint8_t* latent_code;     // Código comprimido
    uint64_t decoder_offset;  // 0 se externo
} cafe_neural_codec_t;
```

**Desafios**:
- Requer GPU para decodificação
- Distribuir decoder neural
- Área de pesquisa ativa

**Potencial Acadêmico**: Paper em CVPR/ICCV/NeurIPS

**Complexidade**: MUITO ALTA (pesquisa, integração neural nets)

---

#### 11.8 Cryptographic Support 🔐

**Prioridade**: BAIXA ⭐ (nicho)
**Duração**: 2 meses
**Impacto**: Compliance (HIPAA, GDPR)

**Features**:
- Encriptação (AES-256-GCM, ChaCha20)
- Assinaturas digitais (Ed25519)
- Key derivation (Argon2, PBKDF2)

**Casos de Uso**:
- Medical imaging (HIPAA)
- Datasets corporativos
- Verificação de autenticidade

**Complexidade**: Média (crypto libraries, key management)

---

#### 11.9 Annotation Rendering 🎨

**Prioridade**: BAIXA ⭐ (nice-to-have)
**Duração**: 1 mês
**Impacto**: QoL para visualização

**Features**:
- Renderizar bboxes, masks, keypoints
- Overlay direto na imagem
- Útil para debugging e demos

**Complexidade**: Baixa (rendering simples)

---

### Timeline de Extensões (Fase 11+)

```
VERSÃO 2.0 (12-15 meses pós-v1.0)
├─ Video/Temporal Support (3-4 meses)
├─ Multi-Modal Data (2-3 meses)
├─ Cloud-Native Optimizations (2-3 meses)
└─ Smart Metadata Indexing (2-3 meses)

VERSÃO 2.1 (3-4 meses após v2.0)
├─ ROI Decoding (2 meses)
└─ Multi-Resolution Pyramid (2-3 meses)

VERSÃO 2.2 (2-3 meses após v2.1)
├─ Cryptographic Support (2 meses) [se demanda]
└─ Annotation Rendering (1 mês)

VERSÃO 3.0 - RESEARCH (TBD)
└─ Neural Codec Support (4-6 meses de pesquisa)
```

---

### Matriz de Decisão para Extensões

| Feature | Quando Implementar | Condição |
|---------|-------------------|----------|
| Video/Temporal | v2.0 | Se comunidade pedir + casos de uso claros |
| Multi-Modal | v2.0 | Se adoção em RGB-D research |
| Cloud-Native | v2.0 | Se uso em cloud significativo |
| Smart Indexing | v2.0 | Se datasets >1M imagens comuns |
| ROI Decoding | v2.1 | Se pedidos para gigapixel/crop |
| Multi-Res Pyramid | v2.1 | Se multi-scale training comum |
| Neural Codec | v3.0 | Se virar projeto de pesquisa |
| Encryption | v2.2 | Se demanda médica/corporativa |
| Annotation Render | v2.2 | Se pedido pela comunidade |

**Nota Importante**: Essas extensões serão implementadas **baseadas em feedback real** após release v1.0. Não faz sentido construí-las antes de saber se o formato core tem adoção.

---

## Princípios para Extensões Futuras

1. **Backward Compatibility**: v2.x deve ler v1.0 perfeitamente
2. **Feature Flags**: Implementações parciais OK (graceful degradation)
3. **Community-Driven**: Priorizar baseado em uso real
4. **Research Opportunities**: Neural codec como projeto acadêmico
5. **Pragmatism**: Não adicionar se não resolver problema real

---

**Plano de Implementação v1.0 - Starting from Zero**<br/>
**Última atualização**: 22 de Fevereiro de 2026 (HDR integrado + Future roadmap)<br/>
**Autor**: Daniel Secco<br/>

*Este plano será atualizado conforme o projeto avança. Extensões futuras estão documentadas mas em standby até v1.0 release.*


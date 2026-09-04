# CAFE — Compression Adaptative Filtering Experiment
## Especificação de Formato de Imagem (v1.2.1)

**Autor:** Daniel Secco<br/>
**Copyright** © 2026 Daniel Secco. Licenciado sob [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/) — ver seção 12.

---

## 1. Visão geral

CAFE é um formato de imagem baseado em chunks (inspirado no PNG), usando **ZSTD** como algoritmo de compressão de bloco, com espaço reservado no formato para suporte a algoritmos adicionais no futuro. O encoder aplica fallback automático para dados brutos quando a compressão não é vantajosa. Suporta canal alfa por padrão, paleta indexada com empacotamento real de índices sub-byte, um conjunto amplo de filtros preditivos por bloco (tile), exibição entrelaçada, decodificação em streaming e metadados de aplicação (EXIF, JSON, ICC, XMP). Suporta HDR ao nível de formato (`Sample format` float/half no `IHDR` + chunk `cHDR`, seção 7), com caminho de extensão para pipeline de cores HDR completo sem quebra de compatibilidade.

Esta versão (v1.2.1) continua com aceleração SIMD agressiva para x86_64 (AVX2), adicionando pack/unpack vetorizado para amostras 1/2/4-bit (speedup 8-16x), expansão/redução de amostras 8→16/32 float (4-6x), byte-shuffle com blocking (melhoria de cache 10-20%), e Filter 3 melhorado (speedup 4-6x). Os 16 filtros preditivos da v1.1 permanecem; a implementação de referência agora inclui 252 testes abrangentes (197 unit + 6 integration roundtrip + 49 SIMD-específicos), zero TODOs/FIXMEs, SIMD feature-gated com detecção automática de CPU e fallback escalar, benchmarking Criterion, e despachante de operador tone-mapping para maior flexibilidade HDR.

---

## 2. Assinatura do arquivo

Todo arquivo `.cafe` começa com 9 bytes fixos:

```
0x89 0x43 0x41 0x46 0x45 0x0D 0x0A 0x1A 0x0A
```

Isso corresponde à sequência `\x89CAFE\r\n\x1a\n` (ASCII puro, sem acento).

Função de cada parte (mesma lógica do PNG):

| Bytes | Função |
|---|---|
| `0x89` | Byte alto — detecta transmissão que trunca bit 7 (modo texto de 7 bits) |
| `0x43 0x41 0x46 0x45` (`CAFE`) | Marca legível do formato |
| `0x0D 0x0A` (`\r\n`) | Detecta corrupção por conversão CRLF↔LF |
| `0x1A` | Ctrl-Z — interrompe `type` de arquivo no Windows/DOS |
| `0x0A` (`\n`) | Detecta corrupção por conversão LF↔CRLF (inverso do `\r\n`) |

---

## 3. Estrutura de chunk

Todo chunk segue o layout:

| Campo | Tamanho | Descrição |
|---|---|---|
| Length | 4 bytes (uint32 BE) | Tamanho do campo `Data` |
| Type | 4 bytes (ASCII) | Identificador do chunk (ex: `IHDR`) |
| Flag | 1 byte | Codec usado nesse chunk — ver enum na seção 3.2 |
| Data | N bytes | Conteúdo do chunk (bruto ou comprimido conforme Flag) |
| CRC32 | 4 bytes | CRC32 sobre `Type + Flag + Data` |

### 3.1 Convenção de nomeação de tipos (crítico vs. ancilar)

O campo `Type` deve conter exatamente 4 caracteres ASCII alfabéticos (`A`–`Z`, `a`–`z`). Nenhum outro byte é permitido nesse campo — é o que garante que a convenção abaixo (baseada em maiúscula/minúscula) seja bem definida para todo chunk, inclusive os que uma implementação futura venha a adicionar.

Seguindo a convenção do PNG:
- **1ª letra maiúscula** → chunk crítico (decoder deve entender ou rejeitar o arquivo)
- **1ª letra minúscula** → chunk ancilar (decoder pode ignorar com segurança se não reconhecer, ou descartar silenciosamente se malformado — ver seção 12.4)

### 3.2 Enum do campo `Flag` e regra de fallback de compressão

O CAFE usa **ZSTD** como algoritmo de compressão de bloco. O campo `Flag` reserva espaço para algoritmos adicionais no futuro, sem exigir mudança na estrutura do formato caso isso se torne necessário — mas nenhum algoritmo além de ZSTD está definido nesta versão.

O encoder testa a compressão e grava o resultado que produzir o menor `Data`, incluindo a opção de gravar bruto se a compressão não comprimir melhor que o original.

**Valores do `Flag`:**

| Valor | Significado |
|---|---|
| `0x00` | Dado bruto (sem compressão) |
| `0x01` | Comprimido com ZSTD |
| `0x02`–`0xFF` | Reservado para algoritmos de compressão futuros |

**Lógica de codificação (aplicável a qualquer chunk compressível):**

```
candidatos = [
    (0x00, dados_originais_do_chunk),
    (0x01, zstd.compress(dados_originais_do_chunk, nivel)),
]

Flag, Data = menor(candidatos, chave=tamanho_em_bytes)
```

Ou seja, o candidato `0x00` (bruto) sempre entra na disputa — se a compressão não resultar em um `Data` menor que o tamanho original, o bloco é gravado sem compressão.

**Nota de interoperabilidade:** o campo `Data` de um chunk com `Flag = 0x01` é um frame ZSTD válido, mas a spec **não exige** que esse frame declare o tamanho do conteúdo descomprimido em seu cabeçalho (o parâmetro `content size`, opcional no formato ZSTD). Diferentes bibliotecas/linguagens têm padrões diferentes quanto a incluir esse campo automaticamente. Um decoder CAFE deve usar uma API de descompressão que **não dependa** desse campo estar presente (ex: leitura em streaming, em vez de uma API "tudo de uma vez" que exija o tamanho antecipado) — caso contrário, arquivos gerados por encoders que omitem esse campo falharão na decodificação sem motivo real relacionado ao formato CAFE em si.

**Nota de segurança:** todo decoder deve impor um limite superior configurável ao tamanho de saída de qualquer descompressão (ver seção 12.2, proteção contra "decompression bomb"). Isso não é opcional — é parte do contrato de decodificação segura do formato.

---

## 4. Chunks definidos

### 4.1 `IHDR` (crítico, sempre primeiro, sempre não-comprimido)

| Campo | Tamanho | Descrição |
|---|---|---|
| Width | 4 bytes | uint32 BE |
| Height | 4 bytes | uint32 BE |
| Bit depth | 1 byte | Bits por canal (ou por índice, em paletas): 1, 2, 4, 8, 10, 12, 16, 32 — ver restrições por color type nas seções 4.1.1 e 4.1.2 |
| Sample format | 1 byte | `0`=uint, `1`=float, `2`=half-float (fp16) |
| Color type | 1 byte | `0`=cinza, `2`=RGB, `3`=indexado (requer PLTE), `4`=cinza+alfa, `6`=RGBA (**padrão**) |
| Compression method | 1 byte | Bitmask dos codecs usados no arquivo — `bit0`=ZSTD, demais bits reservados (0) para algoritmos futuros |
| Filter method | 1 byte | `0`=nenhum, `1`=byte-shuffle (implementado, seção 4.3.2), `2`=preditivo (código por bloco, ver enum completo na seção 4.3.1) |
| Interlace method | 1 byte | `0`=nenhum, `1`=Adam7, `2`=par/ímpar |

> Canal alfa por padrão: `Color type = 6` (RGBA) é o valor recomendado. Se a imagem de origem não tiver alfa, o encoder preenche com `0xFF` (opaco).

> **`Filter method = 1` (byte-shuffle, implementado desde v1.1):** técnica de pré-processamento para amostras multi-byte (bytes por pixel ∈ `{2, 4, 8, 16}`), pensada especialmente para dados float/HDR: reordena os bytes por posição dentro da amostra — todos os bytes menos significativos de todos os pixels primeiro, depois os mais significativos — o que costuma comprimir muito melhor dados de ponto flutuante que a ordem natural intercalada (a correlação entre bytes da mesma posição sobrevive melhor à compressão). O layout exato, restrições e a ordem no pipeline estão na seção 4.3.2. Um decoder antigo que encontrar `Filter method = 1` deve **rejeitar o arquivo explicitamente** (é um campo do `IHDR`, portanto crítico — mesma lógica da seção 5.1 para `Interlace method`), nunca tratá-lo silenciosamente como equivalente a `0` (nenhum filtro).

> **Compression method (bitmask):** o encoder ativa `bit0` se pelo menos um chunk do arquivo usa `Flag = 0x01` (ZSTD). Bits adicionais ficam reservados para algoritmos de compressão futuros. Isso permite que o decoder verifique, logo após ler o `IHDR`, se tem suporte a todos os codecs presentes no arquivo — e rejeite o arquivo imediatamente caso não suporte algum, em vez de falhar no meio da decodificação de um `IDAT`.
>
> **Semântica precisa — declaração de capacidade, não um registro por chunk (normativo):** `compression_method` responde a exatamente uma pergunta: *"quais codecs um decoder precisa suportar para ter alguma chance de decodificar este arquivo?"* Ele **não** é um registro de quais codecs foram de fato usados por algum chunk específico, e **não** substitui o byte `Flag` daquele chunk (seção 3) — os dois campos servem a propósitos diferentes e nenhum pode ser inferido a partir do outro:
>
> - `Flag` (um por chunk) é a única autoridade sobre como o campo `Data` *daquele chunk específico* é codificado (`0x00`=bruto, `0x01`=ZSTD). Um decoder **deve** sempre despachar a descompressão por chunk com base no `Flag`, nunca com base em `compression_method` — `compression_method` é lido uma única vez, do `IHDR`, antes mesmo do `Flag` de qualquer chunk existir para ser inspecionado.
> - `compression_method` (um por arquivo, no `IHDR`) é uma verificação prévia de capacidade, avaliada uma única vez, antes de o decoder se comprometer a interpretar o resto do arquivo. Seu `bit0` é um **limite inferior obrigatório**: um encoder conformante **nunca deve** produzir um arquivo com `bit0 = 0` se algum chunk desse arquivo tiver `Flag = 0x01` — fazer isso permitiria que um decoder concluísse incorretamente, a partir apenas do `IHDR`, que pode decodificar o arquivo sem suporte a ZSTD, apenas para falhar depois em um chunk específico. Esta é uma exigência normativa sobre encoders, independente de o encoder ser em buffer ou em streaming.
> - O inverso é explicitamente **permitido**: `bit0 = 1` **não** garante que algum chunk de fato acabe usando ZSTD (todo chunk ainda pode cair no fallback para armazenamento bruto, conforme a seção 3.2). Declarar uma exigência de capacidade mais ampla do que estritamente necessário é sempre seguro para um decoder aceitar (ver "Superestimativa conservadora" abaixo para o único caso de referência que depende disso).
>
> **Nota de conformidade do decoder:** o decoder de referência só valida que `compression_method` não contém bits desconhecidos/reservados (`compression_method & !0b0000_0001 != 0` é rejeitado) — ele **não** valida cruzadamente o `bit0` contra os bytes `Flag` de fato encontrados ao ler os chunks. Isso significa que um encoder não-conformante que viole a regra do limite-inferior-obrigatório acima (declarando `bit0 = 0` mas ainda assim emitindo um chunk com `Flag = 0x01`) não será detectado pela inspeção de `compression_method` na implementação de referência; o chunk afetado ainda será decodificado corretamente de qualquer forma, porque a descompressão do chunk é despachada a partir do próprio `Flag` daquele chunk, não do `IHDR`. Isso é seguro para o próprio decoder de referência (nenhum resultado de decodificação incorreto, nenhum panic — cada chunk é tratado corretamente pelo seu próprio `Flag` de qualquer forma), mas representa um risco real de interoperabilidade para qualquer decoder independente que trate `compression_method` como autorização para pular a inicialização de um caminho de código com suporte a ZSTD (ex.: para manter uma build `no_std`/embarcada pequena, omitindo a dependência de ZSTD sempre que `bit0 = 0`): tal decoder falharia inesperadamente naquele chunk específico, apesar de o `IHDR` ter afirmado que nenhum suporte a ZSTD era necessário. Implementadores de decoders alternativos que queiram detectar essa violação específica devem comparar `bit0` com os bytes `Flag` de todo chunk que lerem, como uma verificação de integridade adicional além do que esta especificação exige que a implementação de referência execute.
>
> **Superestimativa conservadora para encoders não-seekable (v1.6+, implementação de referência):** um encoder em streaming que escreve o `IHDR` antes de saber se algum chunk posterior de fato usará ZSTD (ou seja, antes de comprimir um único tile) não consegue calcular o valor exato deste bitmask antecipadamente — e, se seu destino de saída não suportar seek de volta, também não consegue corrigi-lo depois. A implementação de referência do `Encoder<W: Write>` (seção 6, encode em streaming) resolve isso ativando `bit0` incondicionalmente e antecipadamente, o que só pode *superestimar* (declarar suporte a ZSTD necessário mesmo que todo chunk tenha caído no fallback para armazenamento bruto), nunca *subestimar* — um decoder que rejeita o arquivo por falta de suporte a ZSTD quando não era realmente necessário é desnecessariamente rígido, mas ainda seguro; um decoder que aceita um arquivo que não consegue de fato descomprimir não é. Esta é a única direção de imprecisão que este campo pode ter e ainda satisfazer seu propósito declarado (verificação prévia de capacidade do decoder). Quando o destino suporta seek (`Encoder<W: Write + Seek>`), a implementação de referência corrige esse byte (e o CRC32 do `IHDR`) para seu valor exato assim que o último chunk é conhecido, idêntico ao caminho `encode()` de arquivo completo — ver seção 6.1.

**Total: 14 bytes de payload no IHDR.**

#### 4.1.1 Regras para `Bit depth = 1` (bilevel/bitmap puro)

Suporta imagens estritamente preto-e-branco (1 bit por pixel, sem tons intermediários — ex: fax, desenho técnico, scans binarizados).

- **Color types permitidos**: `0` (sem alfa) e `4` (com alfa). Quando `4`, o canal alfa também é armazenado como 1 bit por pixel (máscara binária: opaco/transparente, sem meio-termo).
- **Sample format**: deve ser `0` (uint). `bit depth = 1` com `sample format = 1` ou `2` (float) é inválido — decoder deve rejeitar o arquivo.
- **Empacotamento**: pixels são empacotados 8 por byte, MSB primeiro (bit mais significativo = pixel mais à esquerda), igual ao PNG. Se a largura do bloco/tile não for múltipla de 8, os bits restantes do último byte de cada linha são preenchidos com `0` e ignorados na decodificação.
- **Filter method**: `1` (byte-shuffle, seção 4.3.2) não se aplica a `bit depth = 1`, já que não há bytes de amostra multi-byte para reordenar. Os filtros preditivos da seção 4.3.1 (`Filter method = 2`) continuam válidos, operando sobre os bytes já empacotados — não sobre bits individuais.
- **Interlace**: compatível com `Adam7` e par/ímpar, mas o empacotamento de bits é recalculado por passe (cada passe tem sua própria contagem de pixels por linha, logo seu próprio esquema de padding ao final da linha).
- **Fallback de compressão**: aplica-se normalmente (seção 3.2) sobre o buffer já empacotado em bits — ZSTD costuma comprimir bem esse tipo de dado, dada a alta redundância de padrões binários.

#### 4.1.2 Regras para `Color type = 3` (indexado) e chunk `PLTE`

Suporta paletas de cores indexadas (imagens com poucas cores distintas — ícones, pixel art, gráficos, screenshots simples), reduzindo drasticamente o tamanho antes mesmo da compressão ZSTD.

**`PLTE` — chunk crítico, obrigatório quando `Color type = 3`, deve aparecer antes do primeiro `IDAT`:**

| Campo | Tamanho | Descrição |
|---|---|---|
| Entry format | 1 byte | `0` = RGB (3 bytes/entrada), `1` = RGBA (4 bytes/entrada) |
| Entries | N × (3 ou 4 bytes) | Cores da paleta, na ordem de seus índices (0, 1, 2, ...) |

- Com `entry format = 1` (RGBA), cada entrada da paleta já carrega seu próprio canal alfa — não é necessário um chunk `tRNS` separado como no PNG.
- Segue a regra de fallback padrão de compressão (seção 3.2), embora paletas costumem ser pequenas o suficiente para o ganho ser marginal.

**Regras adicionais quando `Color type = 3`:**

- **Bit depth** representa bits por **índice de pixel** (não por canal de cor): valores válidos são `1, 2, 4, 8`. Número máximo de entradas na paleta = `2^bit_depth` (ex: bit depth 8 → até 256 cores).
- **Empacotamento de índices (implementado):** quando `bit depth` é `1`, `2` ou `4`, múltiplos índices são empacotados por byte, **MSB primeiro**, com padding de zeros ao final de cada linha caso a largura não seja múltipla da capacidade do byte — mesma lógica da seção 4.1.1. Bytes por linha = `ceil(width × bit_depth / 8)`. A implementação de referência escolhe automaticamente o menor `bit_depth` que comporta o número de cores da paleta (ex: paleta com até 2 cores → `bit_depth = 1`; até 16 cores → `bit_depth = 4`).
- **Sample format** deve ser `0` (uint). Índice em float é inválido.
- **Filter method**: os filtros preditivos da seção 4.3.1 operam sobre os bytes já empacotados (não sobre índices/bits individuais), válidos para qualquer `bit depth`. O byte-shuffle (seção 4.3.2) **não** se aplica a paletas: índices têm 1 byte/pixel (`bytes per pixel = 1`, fora do conjunto `{2, 4, 8, 16}` exigido), e um encoder não deve gravar `Filter method = 1` com `Color type = 3`.
- Se o decoder encontrar `Color type = 3` sem um chunk `PLTE` precedente, deve rejeitar o arquivo.
- **Nota de segurança (limite de entradas):** como o bit depth (valores válidos `1, 2, 4, 8`) limita o espaço de índices endereçáveis a `2^bit_depth ≤ 256`, um chunk `PLTE` declarando mais entradas do que esse máximo (256, independentemente do `bit depth` realmente em uso) jamais pode ser validamente referenciado por nenhum índice de pixel. Um decodificador não deve confiar apenas no teto genérico de descompressão por chunk (seção 12.2) para limitar essa alocação de `Vec`/array — esse teto foi dimensionado para dados de pixel, não para metadados de paleta, e de outra forma permitiria que um único chunk `PLTE` forjado alocasse ordens de magnitude a mais de memória (até ~4 GiB a partir de dados de entrada de paleta descomprimidos no teto de descompressão de 1 GiB) do que qualquer paleta legítima (máximo 256 × 4 bytes = 1 KiB) jamais precisaria. Decodificadores **devem** rejeitar um chunk `PLTE` cujo número de entradas exceda 256 imediatamente ao calculá-lo a partir do comprimento descomprimido do chunk, antes de alocar armazenamento para as entradas propriamente ditas.
- **Algoritmo de quantização de paleta (apenas do encoder, não faz parte do contrato de decodificação)**: como um encoder escolhe quais cores entram na `PLTE` e para qual entrada da paleta cada pixel é mapeado é uma decisão inteiramente do lado do encoder — o decoder apenas lê a `PLTE` + índices finais que recebe, então isso não tem nenhum efeito sobre a conformidade do formato de arquivo ou interoperabilidade (análogo às heurísticas de seleção de filtro da seção 4.3.1, que também não fazem parte do contrato de decodificação). A implementação de referência oferece três estratégias intercambiáveis: um coletor incremental guloso de vizinho mais próximo (mais rápido, padrão), um algoritmo median-cut (divide recursivamente o espaço de cores para melhor qualidade média), e uma variante de vizinho mais próximo usando uma métrica de distância perceptualmente ponderada ("redmean") em vez da distância euclidiana simples em suas comparações de correspondência mais próxima (v1.5) — essa última pondera a contribuição de cada canal com base no nível médio de vermelho das cores comparadas (ver <https://www.compuphase.com/cmetric.htm>), uma aproximação barata da percepção humana de cor que tende a produzir menos incompatibilidades visualmente perceptíveis do que a distância euclidiana não ponderada, particularmente para paletas/imagens com cores próximas aos extremos de vermelho ou azul.

#### 4.1.3 Layout de bytes do pixel

Regras gerais de como os bytes de um pixel/linha/amostra são organizados, aplicáveis a todos os color types:

- **Ordem dos canais dentro do pixel**: `Color type = 6` (RGBA) armazena os canais na ordem **R, G, B, A**. `Color type = 2` (RGB) armazena **R, G, B**. `Color type = 4` (cinza+alfa) armazena **Cinza, Alfa**. `Color type = 0` (cinza) armazena apenas o canal de cinza. `Color type = 3` (indexado) armazena o índice de pixel (não há canais de cor diretos — a cor vem da `PLTE`, seção 4.1.2).
- **Ordem das linhas**: linha `0` é a linha **superior** da imagem/tile; as linhas seguem top-to-bottom, mesma convenção do PNG.
- **Endianness de amostras multi-byte**: qualquer amostra com mais de 8 bits (`bit depth = 10, 12, 16, 32`) é armazenada em **big-endian**, consistente com todos os demais campos multi-byte do formato (`Width`, `Height`, `Length`, `CRC32`). Isso também vale para os campos float do chunk `cHDR` (seção 4.4) — todos em IEEE 754 big-endian.
- **Empacotamento de `bit depth = 10` e `12`**: cada amostra ocupa um contêiner de **16 bits inteiros** (2 bytes, big-endian), com os bits mais significativos não utilizados preenchidos com `0`. Ou seja, uma amostra de 12 bits ocupa os 12 bits menos significativos de um `uint16`, e os 4 bits mais significativos são sempre `0`. Essa opção prioriza simplicidade de implementação (mesma lógica de leitura que `bit depth = 16`) sobre densidade máxima de bits — o espaço "desperdiçado" antes da compressão é, na prática, absorvido pelo ZSTD, então o custo real após compressão é pequeno.

### 4.2 `iDIM` (ancilar, opcional, define particionamento para streaming)

| Campo | Tamanho | Descrição |
|---|---|---|
| Tile width | 2 bytes | Largura do tile em pixels |
| Tile height | 2 bytes | Altura do tile em pixels |
| Tiles X | 2 bytes | Número de tiles na horizontal |
| Tiles Y | 2 bytes | Número de tiles na vertical |
| Scan order | 1 byte | `0`=linha a linha, `1`=Z-order (Morton) |

Se ausente, o decoder assume um único chunk `IDAT` cobrindo a imagem inteira (sem tiling), ou tiles de linhas de tamanho arbitrário definido internamente pelo encoder (ver seção 4.3 — o particionamento em tiles de linhas não exige `iDIM`; ele só é necessário para tiling 2D real ou para sinalizar a ordem de streaming).

**Ordem de aparição dos `IDAT`:** quando o encoder usa tiling 2D real (não apenas faixas de linhas), os chunks `IDAT` aparecem no arquivo na mesma ordem definida por `Scan order`, sem necessidade de um índice explícito por chunk:

- `Scan order = 0` (linha a linha): tiles em ordem row-major — esquerda→direita dentro de cada linha de tiles, depois cima→baixo (primeiro todos os tiles da linha `tile_y = 0`, depois `tile_y = 1`, e assim por diante).
- `Scan order = 1` (Z-order/Morton): tiles ordenados pelo código Morton (bits de `tile_x` e `tile_y` intercalados) de suas coordenadas `(tile_x, tile_y)` na grade — mesma lógica usada para preview progressivo por região espacial (seção 6).

O N-ésimo chunk `IDAT` do arquivo (contando apenas chunks `IDAT`, 0-indexado) corresponde à N-ésima posição dessa ordem de enumeração.

**Tiles de borda (largura/altura não múltiplas do tile):** não há padding. Quando `width` não é múltiplo de `Tile width`, os tiles da última coluna (`tile_x = Tiles X - 1`) têm largura real `width − (Tiles X − 1) × Tile width` (menor que `Tile width`). O mesmo vale para a última linha de tiles (`tile_y = Tiles Y - 1`) em relação a `height` e `Tile height`. O decoder calcula essas dimensões reduzidas a partir de `Width`/`Height` (IHDR) e `Tile width`/`Tile height`/`Tiles X`/`Tiles Y` (iDIM) — nenhum campo adicional por tile é necessário.

**Nota de segurança (limite de contagem de tiles):** `Tiles X` e `Tiles Y` são cada um individualmente válidos como qualquer `u16` não-zero (1-65535), e um decodificador deve reconciliar seus valores declarados com `Width`/`Height`/`Tile width`/`Tile height` (rejeitando qualquer inconsistência, conforme seção 12.1) — mas o *produto* deles não tem nenhum teto inerente apenas por essa checagem de consistência (ex: `Tile width = Tile height = 1` com `Width = Height = 65535` produz um consistente `Tiles X = Tiles Y = 65535`, ou seja, ~4,29 bilhões de tiles). Como a ordem de visitação dos tiles (`tile_order()` na implementação de referência) é calculada uma única vez por arquivo a partir apenas de `Tiles X × Tiles Y` — independentemente de, e antes de, qualquer `IDAT` ser lido — um decodificador que não limita adicionalmente esse produto fica vulnerável a uma negação de serviço por exaustão de memória a partir de um único arquivo de ~70 bytes (`IHDR` + `iDIM`, nenhum `IDAT` necessário para disparar). Decodificadores **devem** impor um teto superior finito sobre `Tiles X × Tiles Y` (a implementação de referência usa 1.048.576 — ver `MAX_TILE_COUNT` em `src/constants.rs` — escolhido para exceder confortavelmente qualquer caso de uso realista de streaming/tiling) e rejeitar o arquivo com um erro tratável caso excedido, antes de calcular a ordem dos tiles ou alocar qualquer coisa proporcional à contagem de tiles.

### 4.3 `IDAT` (crítico, um ou mais por arquivo)

Contém os pixels (ou índices de paleta) de um bloco/tile da imagem. Cada `IDAT` é **independente** — pode ser comprimido ou não (regra de fallback da seção 3.2), e decodificado assim que chega (streaming).

**Ordem de operações na codificação de um `IDAT`:**

```
pixels crus do bloco/tile (ou índices de paleta empacotados, seção 4.1.2)
    → (se Filter method = 2) aplicar filtro preditivo por bloco (seção 4.3.1)
    → (se Filter method = 1) aplicar byte-shuffle por bloco/tile (seção 4.3.2)
    → (se interlace ≠ 0) prefixar pass_number
    → aplicar regra de fallback de compressão (seção 3.2), opcionalmente com
      dicionário ZSTD (seção 4.9, zDIC) se presente no arquivo
```

**Payload antes da compressão** (quando interlace ≠ 0):

```
[pass_number: 1 byte][linhas do bloco, já filtradas se Filter method = 2]
```

Quando interlace = 0, o payload é diretamente as linhas do bloco/tile (já filtradas se aplicável), sem cabeçalho adicional além do que a seção 4.3.1 descreve.

O entrelaçamento (Adam7 e par/ímpar, seção 5) se aplica tanto a imagens RGBA diretas quanto a imagens com paleta indexada — nesse último caso, a implementação de referência converte os índices para RGBA antes de entrelaçar, já que os 7 passes do Adam7 (de resoluções diferentes) não se combinam de forma direta com paletas de tamanho variável por passe.

#### 4.3.1 Filtro preditivo (`Filter method = 2`)

Reduz a entropia dos dados **antes** da compressão, prevendo o valor de cada byte de pixel (ou índice) a partir de vizinhos já conhecidos e armazenando apenas o resíduo (diferença). É uma técnica de pré-processamento, não de compressão — atua em conjunto com o ZSTD da seção 3.2, não no lugar dele.

**Escolha por bloco (tile inteiro):** um único filtro é escolhido para todo o bloco (o conjunto de linhas que compõe um `IDAT`); todas as linhas do bloco compartilham o mesmo preditor. Os primeiros 5 são os clássicos do PNG; os demais são adições do CAFE:

| Código | Filtro | Predição usada (por byte de amostra) |
|---|---|---|
| `0` | None | Nenhuma — byte original mantido |
| `1` | Sub | Byte do pixel à esquerda (`L`), mesma linha |
| `2` | Up | Byte do pixel de cima (`U`), mesma coluna, linha anterior |
| `3` | Average | Média (`L`, `U`), arredondada para baixo |
| `4` | Paeth | Esquerda, cima ou diagonal superior-esquerda (`UL`) — escolhido pelo preditor de Paeth |
| `5` | MED (Median Edge Detector) | `L`, `U` e `UL` — ver fórmula abaixo |
| `6` | Gradiente (Plane) | `L + U − UL`, sem clamping — ver fórmula abaixo |
| `7` | Simple Median | Mediana simples dos 3 vizinhos (`L`, `U`, `UL`), sem lógica de borda do MED |
| `8` | 2nd Order | `(2×L − LL + 2×U − UU) / 2`, com clamping em `[0, 255]` — ver fórmula abaixo |
| `9` | 4-way Directional (Horizontal) | `(3×L + U) / 4` — favorece continuidade horizontal |
| `10` | 4-way Directional (Vertical) | `(L + 3×U) / 4` — favorece continuidade vertical |
| `11` | 4-way Directional (Diagonal `\`) | `(L + U + 2×UL) / 4` |
| `12` | 4-way Directional (Diagonal `/`) | `(2×L + 2×U + UL) / 5` |
| `13` | Context-Based | Detecta orientação local por gradiente e escolhe dinamicamente entre Sub, Up ou Average — ver fórmula abaixo |
| `14` | TR-Directional | Média bilinear dos 4 vizinhos (`L`, `UL`, `U`, `TR`) — WebP "Predictor 10" (adicionado em v1.1) — ver fórmula abaixo |
| `15` | Weighted (adaptativo) | Média ponderada adaptativa de `L`, `U`, `UL`, `TR` — inspirado no preditor ponderado do JPEG-XL (adicionado em v1.1) — ver fórmula abaixo |

**Fórmula do MED** (mesmo preditor usado por JPEG-LS e FFV1):

```
se UL >= max(U, L):
    predição = min(U, L)
senão se UL <= min(U, L):
    predição = max(U, L)
senão:
    predição = U + L - UL
```

**Fórmula do Gradiente/Plane** (um dos 7 modos clássicos do JPEG Lossless):

```
predição = (U + L − UL) mod 256
```

**Fórmula do Context-Based:**

```
dh = |L − UL|   (diferença horizontal)
dv = |U − UL|   (diferença vertical)

se dh > dv:  predição = L   (borda vertical detectada → filtro Sub)
senão se dv > dh:  predição = U   (borda horizontal detectada → filtro Up)
senão:  predição = (L + U) / 2   (região homogênea → Average)
```

**Fórmula do TR-Directional** (filtro `14`, WebP "Predictor 10"; adicionado em v1.1):

É o único filtro do formato que consome o vizinho superior-direito (`TR`): o byte do pixel à direita, na linha anterior (`x + bpp`). Ausente na borda direita do tile, `TR` é tratado como zero (mesma convenção de zero dos demais vizinhos ausentes).

```
avg2(p, q) = (p + q) >> 1          (média truncada, sem overflow — aritmética em 16 bits)
predição   = avg2( avg2(L, UL), avg2(U, TR) )
```

A combinação bilinear dá o dobro de peso à informação diagonal que o Average (`3`) ignora, tornando-o útil para curvatura suave e gradientes diagonais `/` que nenhum preditor de `L`/`U`/`UL` captura.

**Fórmula do Weighted** (filtro `15`, inspirado no preditor ponderado do JPEG-XL; adicionado em v1.1):

Mantém um estado de pesos `w[L], w[U], w[UL], w[TR]` — inteiros em `[0, 64]`, inicialmente `8` cada — reiniciado por bloco e persistente entre as linhas do bloco. A cada byte, na ordem de varredura (esquerda→direita, topo→baixo):

```
Σw     = w[L] + w[U] + w[UL] + w[TR]
predição = clamp( ( w[L]·L + w[U]·U + w[UL]·UL + w[TR]·TR + Σw/2 ) / Σw , 0, 255 )
```

Após conhecer o valor real `v` (byte original no encoder; `resíduo + predição` no decoder — os dois lados obtêm o mesmo `v`), o estado é atualizado, recompensando o vizinho que mais se aproximou:

```
e_i = |v − vizinho_i|   (para cada um dos 4 vizinhos)
média = (e_L + e_U + e_UL + e_TR) / 4

se e_i ≤ média:  w_i = min(w_i + 1, 64)   (vizinho recompensado)
senão:           w_i = max(w_i − 1, 0)    (vizinho penalizado)
```

Com pesos uniformes (`8` cada) o filtro reduz-se à média simples dos 4 vizinhos; à medida que um vizinho domina localmente, a predição tende a ele. Como o estado evolui de forma determinística a partir de valores já reconstruídos (causais), o decoder reproduz exatamente a mesma predição — **custo de 0 bits extras**.

**Fórmula do 2nd Order** (primeiro filtro a usar vizinhos além de `L`, `U`, `UL` — estende o contexto temporal horizontal e vertical):

Além dos três vizinhos já usados pelos demais filtros, o 2nd Order também usa:
- `LL`: byte duas posições à esquerda, mesma linha (`x − 2×bpp`)
- `UU`: byte da mesma coluna, duas linhas acima (`x` na linha `y − 2`)

```
pred_h = 2×L − LL   (extrapolação linear da tendência horizontal)
pred_v = 2×U − UU   (extrapolação linear da tendência vertical)
predição = clamp((pred_h + pred_v) / 2, 0, 255)
```

Diferente dos demais filtros — que apenas combinam ou escolhem entre vizinhos existentes (portanto já naturalmente limitados a `[0, 255]`) — a extrapolação linear pode, matematicamente, produzir valores fora desse intervalo (ex: uma sequência crescente `LL=0, L=255` extrapola para `510`). Por isso o 2nd Order aplica `clamp` explícito ao resultado antes de convertê-lo para `u8`, em vez de wraparound. Isso **não compromete a reversibilidade**: o resíduo ainda é calculado como `byte_original − predição` (com wraparound módulo 256, seção seguinte) e revertido como `resíduo + predição` — como a função de predição é determinística e usa apenas vizinhos já conhecidos/causais tanto na codificação quanto na decodificação, produz exatamente o mesmo valor nos dois lados, independente de ter usado clamp ou wraparound internamente.

Genuinamente distinto do Average (filtro `3`): o Average prediz a média estática de `L` e `U`; o 2nd Order prediz a **continuação da tendência local**, favorecendo regiões com gradientes suaves e consistentes (rampas, sombreados contínuos) em vez de regiões planas.

Todos os filtros calculam o resíduo final (`byte_original − predição`) e o revertem (`resíduo + predição`) usando aritmética inteira com wraparound módulo 256 (`u8` wrapping), consistente entre codificação e decodificação — isso vale mesmo para o 2nd Order, cujo clamp interno afeta apenas o cálculo da predição em si, não a etapa de resíduo.

**Cada bloco (tile) filtrado é prefixado por 1 byte** indicando qual código foi usado nele (um único código para todas as linhas do bloco). O decoder apenas reverte a operação indicada por esse byte — **a heurística usada pelo encoder para escolher o filtro não faz parte do contrato de decodificação** e pode variar entre implementações sem afetar interoperabilidade. Heurísticas conhecidas, em ordem crescente de custo computacional e qualidade de resultado:

| Heurística | Custo | Observação |
|---|---|---|
| Soma dos valores absolutos dos resíduos (MSAD) | Baixo | Heurística clássica do PNG; assume que resíduo pequeno comprime bem, o que nem sempre é verdade |
| Entropia de Shannon (ordem zero) dos resíduos | Baixo/médio | Captura repetição de padrão, mais alinhada ao que o estágio de entropia do ZSTD explora; recomendada como padrão |
| Compressão de teste real (comprimir cada candidato e comparar o tamanho final) | Alto | Resultado ótimo, mas caro — apropriado para um modo de compressão máxima opcional, não para uso padrão |
| QuickPrune (v1.1) | Baixo/médio | MSAD rápido seguido de Entropia de Shannon nos top 8 candidatos; ~1-2% de ganho de compressão com overhead modesto |
| AdaptiveEntropy (v1.1) | Médio | Análise de tipo de bloco (Suave/Natural/AltaFreq/Misto) + seleção de filtro consciente do conteúdo; ~2-3% de ganho em fotos naturais |

Um encoder é livre para implementar qualquer uma dessas (ou outra) sem quebrar compatibilidade com nenhum decoder CAFE, desde que grave corretamente o código do filtro efetivamente usado em cada bloco.

**Bytes por pixel (bpp)**, usado para localizar o "vizinho à esquerda": `bpp = bytes_por_amostra × canais`, onde `bytes_por_amostra` é `1` para `bit depth ≤ 8` (incluindo `1, 2, 4` empacotados — nesse caso o "canal" já é o byte empacotado, seção 4.1.1/4.1.2), `2` para `bit depth = 10, 12, 16` (contêiner de 16 bits, seção 4.1.3), e `4` para `bit depth = 32`. Mínimo `bpp = 1`.

**Bordas de tile:** como cada `IDAT` é independente (streaming), o vizinho "de cima" só existe se a linha **não for a primeira do tile** — na primeira linha de cada tile, o filtro trata o vizinho de cima como todo-zero, mesma convenção do PNG para a primeira linha da imagem inteira. Isso reduz um pouco a eficiência nas bordas de tiles pequenos, mas preserva a independência exigida pelo streaming (seção 6). O mesmo princípio se aplica aos vizinhos estendidos do filtro 2nd Order (`8`): `LL` é tratado como zero quando não há duas colunas à esquerda disponíveis (`x < 2×bpp`), e `UU` é tratado como zero quando não há duas linhas acima disponíveis dentro do tile (primeira **ou** segunda linha do tile). Os vizinhos superior-direitos dos filtros `14` e `15` seguem a mesma convenção: `TR` é tratado como zero na borda direita do tile (`x + bpp ≥ bytes_por_linha`) ou na primeira linha. O estado adaptativo do filtro Weighted (`15`) é reiniciado no início de cada bloco/tile e compartilhado entre todas as linhas do bloco — o decoder deriva o mesmo estado na mesma ordem de varredura.

#### 4.3.2 Byte-shuffle (`Filter method = 1`) — implementado na referência desde v1.1

Reduz a entropia de dados de **amostras multi-byte** antes da compressão, reordenando os bytes por posição dentro da amostra. É uma técnica de pré-processamento (não de compressão), como o filtro preditivo da seção 4.3.1, e atua em conjunto com o ZSTD da seção 3.2.

**Quando usar:** pensado principalmente para `Sample format = 1` (float) ou `2` (half-float) e `bit depth` altos, onde a ordem natural intercalada (byte 0 do canal R, byte 0 do canal G, ...) mistura bytes de significância diferente no mesmo bloco comprimido. Ao agrupar todos os bytes de mesma posição (ex: todos os bytes menos significativos de todos os pixels, depois os mais significativos), a correlação entre bytes adjacentes sobe e o ZSTD comprime melhor.

**Transformação (lossless, bijetiva):** para um bloco/tile com `P = largura × altura` pixels e `bpp` bytes por pixel, o buffer natural é

```
[byte0_p0, byte1_p0, …, byte(bpp−1)_p0, byte0_p1, …, byte(bpp−1)_p(P−1)]
```

e o buffer embaralhado agrupa por posição de byte:

```
[byte0_p0, byte0_p1, …, byte0_p(P−1), byte1_p0, byte1_p1, …, byte(bpp−1)_p(P−1)]
```

**Restrições (encoder e decoder devem validar):**

- `bpp ∈ {2, 4, 8, 16}`. Fora desse conjunto, `Filter method = 1` é inválido — o encoder deve recusar gravar e o decoder deve rejeitar o arquivo (ou tratar como erro de formato). Isso exclui `Color type = 3` (indexado, `bpp = 1`) e `bit depth < 8` (amostras sub-byte não têm bytes de amostra para reordenar). Em particular, `bpp = 1`, `3`, `6`, `12` não são suportados.
- **Sem prefixo de byte de filtro**: ao contrário do filtro preditivo (que prefixa 1 byte por bloco), o byte-shuffle **não** adiciona nenhum byte de cabeçalho — o payload do `IDAT` é exatamente o buffer embaralhado.
- **Incompatível com interlace** (`Interlace method ≠ 0`): interlace requer RGBA uint de 8 bits (`bpp = 4`), e o pipeline de passes não combina com o reordenamento — encoder deve rejeitar a combinação.
- **Aplicado por bloco/tile**: cada `IDAT` é embaralhado (e desembaralhado) independentemente, com as dimensões daquele tile (`largura × altura` do tile, não da imagem inteira) — preserva a independência de streaming da seção 6. O decoder deriva a altura do tile a partir do tamanho do payload descomprimido (`tiles_do_bloco = len_payload / bytes_por_linha`), e as dimensões do tile vêm do `iDIM` no caso de tiling 2D.
- **Ordem no pipeline**: o byte-shuffle é aplicado **antes** do fallback de compressão (seção 3.2) e depois de qualquer empacotamento de amostras; não é combinado com o filtro preditivo (são mutuamente exclusivos — apenas um `Filter method` é gravado no `IHDR`).

**Decodificação:** reverter a transformação é a operação inversa bijetiva, sem perda. O decoder valida `bpp`, dimensões e tamanho do buffer antes de desembaralhar (as mesmas checagens da seção 12.1), e desembaralha antes de qualquer filtro preditivo — na prática, antes da conversão de cor (seção 4.1.3) e, para HDR, antes do tone mapping (seção 7).

### 4.4 `cHDR` (ancilar, opcional — metadados HDR — **implementado na referência desde v1.0**)

> **Status:** metadados HDR suportados na implementação de referência desde v1.0 (encode via `EncodeOptions.chdr_metadata` / flags `--chdr-*` do CLI; decode expõe `DecodeResult.chdr_metadata`). Chunks `cHDR` malformados são descartados silenciosamente, conforme seção 12.4.

| Campo | Tamanho | Descrição |
|---|---|---|
| Transfer function | 1 byte | `0`=linear, `1`=PQ (SMPTE 2084), `2`=HLG, `3`=sRGB/gamma |
| Color primaries | 1 byte | `0`=sRGB/BT.709, `1`=BT.2020, `2`=DCI-P3 |
| Max luminance | 4 bytes float | Em nits |
| Min luminance | 4 bytes float | Em nits |
| MaxCLL | 4 bytes (opcional) | Content Light Level máximo |
| MaxFALL | 4 bytes (opcional) | Frame Average Light Level máximo |

**Presença dos campos opcionais:** o decoder determina se `MaxCLL` e/ou `MaxFALL` estão presentes a partir do campo `Length` do chunk (seção 3): `Length = 10` → apenas os campos obrigatórios; `Length = 14` → obrigatórios + `MaxCLL`; `Length = 18` → obrigatórios + `MaxCLL` + `MaxFALL`. Não é permitido incluir `MaxFALL` sem `MaxCLL` (ordem fixa); um `Length` que não corresponda a nenhuma dessas três combinações é inválido e o chunk deve ser rejeitado (mas, por ser ancilar, isso não invalida o arquivo inteiro).

**Espaço de cor padrão (na ausência de `cHDR`):** na ausência do chunk `cHDR` (e do chunk `iCCP`, seção 4.7), todos os valores RGB do CAFE devem ser interpretados como **sRGB (IEC 61966-2-1)**. Isso remove qualquer ambiguidade sobre como exibir as cores de um arquivo que não traz metadados de cor explícitos — o comportamento padrão é bem definido, não "indefinido" ou "dependente do decoder", exatamente como o PNG assume sRGB na ausência de `gAMA`/`cHRM`/`iCCP`. Um decoder que não implementa `cHDR`/`iCCP` está, portanto, sempre correto ao tratar as cores como sRGB.

### 4.5 `eXIF` (ancilar, opcional, instância única — metadados EXIF)

Armazena metadados EXIF (câmera, data de captura, geolocalização, orientação, etc.) usando o padrão já definido pela especificação Exif da CIPA — mesma abordagem adotada pelo PNG desde 2017 (chunk `eXIf`).

| Campo | Tamanho | Descrição |
|---|---|---|
| Payload | resto do `Data` | Blob EXIF bruto, no formato TIFF completo (incluindo seu próprio cabeçalho de byte order, `II*\0` ou `MM\0*`), exatamente como definido pela especificação Exif |

- O CAFE **não interpreta nem transforma** o conteúdo — é armazenado como um blob opaco, byte a byte, conforme a especificação Exif define.
- **Compressão**: segue a regra de fallback padrão (seção 3.2).
- **Instância única por arquivo**: um decoder que encontrar mais de uma instância deve considerar apenas a primeira e ignorar as demais.
- **Posição recomendada**: antes do primeiro `IDAT`.
- **Ancilar**: um decoder que não interpreta EXIF simplesmente ignora o chunk e continua lendo a imagem normalmente — nenhuma informação de pixel depende dele.

### 4.6 `jSON` (ancilar, opcional, múltiplas instâncias permitidas — metadados JSON de aplicação)

Armazena metadados arbitrários definidos por aplicações ou usuários, em formato JSON, quando o dado não se encaixa em nenhum chunk padronizado (ex: `eXIF`) — como histórico de edição, tags de catálogo, proveniência, texto alternativo de acessibilidade, ou dados de pipeline de uma ferramenta específica.

**Payload (antes da compressão):**

| Campo | Tamanho | Descrição |
|---|---|---|
| Namespace length | 1 byte | Tamanho do campo Namespace |
| Namespace | N bytes | String ASCII (ex: `"app.editor"`, `"catalog.tags"`, `"user"`) |
| JSON payload | resto do `Data` | Texto UTF-8 válido, estrutura livre |

- **Namespace** evita colisão entre metadados de diferentes origens (ferramentas, plugins, usuário final). Limite de 255 bytes.
- **Múltiplas instâncias de `jSON`** são permitidas no mesmo arquivo, cada uma com seu próprio namespace. Decoders devem filtrar pelo namespace que reconhecem e ignorar os demais.
- **Compressão**: segue a regra de fallback padrão (seção 3.2) — JSON tende a comprimir bem com ZSTD.
- **Posição recomendada**: após `eXIF` (se presente) e antes do primeiro `IDAT`.
- **Validação**: um `jSON` malformado (namespace length inconsistente, ou JSON inválido) **não invalida o arquivo nem deve causar erro fatal** de decodificação, por ser ancilar — o decoder descarta apenas aquele bloco específico e continua (ver seção 12.4).
- **Recomendação de uso**: manter o payload pequeno (poucos KB). Para blobs binários grandes definidos pelo usuário, prefira um chunk binário dedicado em vez de JSON.

#### 4.6.1 Convenção para comentários de texto simples

O CAFE **não define um chunk dedicado a comentários de texto livre** (uma versão anterior desta spec previu um chunk `cOMM` separado, mas ele foi descartado por se sobrepor inteiramente ao `jSON`). Em vez disso, a convenção recomendada é usar o namespace **`"comment"`** dentro de um chunk `jSON`:

```json
{"text": "Criado com CAFE Encoder v1.0", "author": "opcional"}
```

Isso evita ter dois mecanismos de metadados de texto livre concorrentes no formato, mantendo o `jSON` como o único ponto de entrada para metadados de aplicação não-padronizados.

### 4.7 `iCCP` (ancilar, opcional, instância única — perfil de cor ICC)

Armazena um perfil de gerenciamento de cores ICC (International Color Consortium), para workflows que exigem reprodução de cor precisa além do sRGB padrão.

| Campo | Tamanho | Descrição |
|---|---|---|
| Payload | resto do `Data` | Perfil ICC bruto (binário), formato definido pela especificação ICC |

- O CAFE **não interpreta** o conteúdo do perfil — é armazenado como blob opaco.
- **Compressão**: segue a regra de fallback padrão (seção 3.2).
- **Instância única por arquivo.**
- **Interação com `cHDR`**: se ambos os chunks estiverem ausentes, aplica-se a regra de sRGB padrão da seção 4.4. Se `iCCP` estiver presente, ele tem precedência sobre a suposição de sRGB.
- **Posição recomendada**: antes do primeiro `IDAT`.

### 4.8 `xMPd` (ancilar, opcional, instância única — metadados XMP)

Armazena metadados no formato XMP (Extensible Metadata Platform, padrão Adobe/ISO 16684-1) — útil para compatibilidade com pipelines editoriais e de gerenciamento de ativos digitais que já usam XMP em outros formatos (JPEG, TIFF, PNG).

| Campo | Tamanho | Descrição |
|---|---|---|
| Payload | resto do `Data` | XML UTF-8 válido, conforme especificação XMP |

- **Compressão**: segue a regra de fallback padrão (seção 3.2) — XML tende a comprimir bem.
- **Instância única por arquivo.**
- **Posição recomendada**: antes do primeiro `IDAT`.
- Sobreposição com `jSON`/`eXIF`: é esperado que aplicações escolham **um** mecanismo de metadados por tipo de dado (EXIF para dados de captura, XMP para fluxo editorial, JSON para dados proprietários de aplicação) — o CAFE não impõe qual usar, apenas fornece os três.

### 4.9 `zDIC` (ancilar, opcional, instância única — dicionário ZSTD)

Armazena um dicionário ZSTD usado para **melhorar a compressão de chunks `IDAT`** — especialmente valioso em imagens pequenas ou com padrões repetitivos que se beneficiam de um dicionário pré-treinado (ex: `zstd::dict::from_samples`), já que blocos pequenos individualmente não dão ao ZSTD contexto suficiente para comprimir bem sozinhos.

| Campo | Tamanho | Descrição |
|---|---|---|
| Payload | resto do `Data` | Dicionário ZSTD bruto (formato definido pela biblioteca ZSTD — pode ser um dicionário treinado formalmente, com `Dictionary_ID` embutido, ou um "dicionário de conteúdo bruto" sem essa formalidade) |

- **Escopo**: o dicionário se aplica **apenas aos chunks `IDAT`** do arquivo — não a `eXIF`, `jSON`, `iCCP` ou `xMPd`, que continuam usando ZSTD sem dicionário. Essa restrição de escopo simplifica a implementação e mantém os metadados decodificáveis independentemente da presença/ausência do `zDIC`.
- **Uso real na compressão (funcional, não decorativo)**: quando presente, o decoder deve fornecer esse dicionário ao descompressor ao processar qualquer `IDAT` com `Flag = 0x01` (ZSTD). Nem todo `IDAT` nessas condições precisa ter sido de fato comprimido *com* o dicionário — um dicionário configurado no decoder é ignorado de forma transparente pelo ZSTD ao descomprimir um frame que foi comprimido sem ele, e usado normalmente quando o frame foi comprimido com ele, já que os cabeçalhos de frame ZSTD auto-descrevem o uso de dicionário (ver a garantia de não-regressão do encoder abaixo, que depende exatamente dessa propriedade para misturar `IDAT`s com e sem dicionário no mesmo arquivo quando vantajoso).
- **Posição obrigatória**: antes do primeiro `IDAT` (o decoder precisa do dicionário já disponível antes de processar qualquer chunk que dependa dele).
- **Comportamento com dicionário formalmente treinado**: se o dicionário foi gerado com uma ferramenta de treinamento formal (contém `Dictionary_ID`), a descompressão **falha explicitamente** (`Dictionary mismatch`) caso o decoder tente usar um dicionário diferente ou nenhum — isso é uma propriedade do próprio formato ZSTD, não algo que o CAFE precisa implementar por conta própria.
- **Instância única por arquivo.**
- **Garantia de não-regressão do encoder para dicionários auto-treinados (v1.5)**: um dicionário treinado automaticamente a partir dos próprios dados da imagem (em contraste com um fornecido explicitamente pelo chamador) só deve ser emitido — e só deve ser usado para comprimir `IDAT`s — quando isso produzir um arquivo estritamente menor do que não usar dicionário algum. Isso não é um requisito do lado do decoder (um chunk `zDIC`, uma vez presente, é sempre usado exatamente como descrito acima); é uma recomendação para encoders que treinam dicionários automaticamente: um frame ZSTD em modo dicionário carrega overhead fixo por frame que um frame comum não tem, e esse overhead pode superar o ganho de compressão obtido pelos acertos do dicionário em tiles pequenos ou muito redundantes — medido em até ~78% de saída *maior* em conteúdo sintético repetitivo quando essa precaução não é tomada. Um encoder que implemente essa garantia deve comparar a compressão com e sem o dicionário por `IDAT`, e adicionalmente comparar o total do arquivo inteiro (o tamanho do próprio chunk `zDIC` somado a todos os `IDAT`s) contra o encode equivalente sem dicionário, mantendo o que for menor. Um dicionário fornecido pelo chamador (isto é, não auto-treinado) fica isento dessa recomendação, já que o chamador fez uma escolha deliberada de usá-lo (ex: um dicionário compartilhado treinado offline entre um lote de imagens relacionadas).

### 4.10 `IEND` (crítico, marca fim do arquivo)

`Length = 0`. Sem `Data`.

---

## 5. Entrelaçamento (interlacing)

- `Interlace = 1` (Adam7): 7 passes progressivos, mesma lógica do PNG, adaptada para produzir um `IDAT` por combinação de (tile, passe).
- `Interlace = 2` (par/ímpar): 2 passes, menor complexidade de implementação, ganho de UX mais simples.
- `Interlace = 0`: sem entrelaçamento, decodificação linear top-to-bottom.

Cada `IDAT` inclui o `pass_number` no início do payload (antes da compressão) quando interlace ≠ 0. O entrelaçamento é aplicável tanto ao caminho RGBA direto quanto ao caminho com paleta indexada (nesse último caso, via conversão intermediária para RGBA — ver seção 4.3).

### 5.1 Decisão de design: por que não há um quarto modo de entrelaçamento

A spec propositalmente **não** adiciona um `Interlace = 3` (ou qualquer outro esquema além dos três acima), mesmo que existam variações plausíveis (ex: um esquema tipo pirâmide/mipmap com duplicação de resolução por passe).

Motivo: `Interlace method` é um campo do `IHDR` — portanto **crítico**, não ancilar. Um decoder que não reconhece o valor não consegue decodificar a imagem de forma alguma; não é uma feature que se degrada graciosamente, é um arquivo ilegível para aquele decoder. Isso é fundamentalmente diferente de adicionar um chunk novo (como `iCCP` ou `xMPd`), que um decoder desatualizado pode ignorar com segurança e ainda assim ler a imagem.

Cada novo modo de entrelaçamento é, na prática, uma obrigação de implementação para **todo** decoder que queira se dizer compatível com CAFE — a complexidade de conformidade cresce combinatoriamente (contra bit depth, color type, filtro preditivo, paleta), não linearmente.

Os três valores existentes já cobrem os casos de uso reais:

| Necessidade | Já resolvido por |
|---|---|
| Preview borrado da imagem inteira, refinando por resolução | `Interlace = 1` (Adam7) |
| Refinamento simples, menor overhead de implementação | `Interlace = 2` (par/ímpar) |
| Preview progressivo por região espacial (não por resolução) | `iDIM` com `scan_order = 1` (Z-order) — já ancilar, sem risco de fragmentação |

Se no futuro surgir uma necessidade real de um preview espacial diferente, o caminho recomendado é estender `scan_order` no chunk `iDIM` (ancilar), não adicionar um novo `Interlace method` (crítico).

---

## 6. Streaming

Requisitos para decodificação incremental:

1. `IHDR` sempre é o primeiro chunk, pequeno e não-comprimido — dimensões e formato ficam disponíveis imediatamente.
2. `iDIM`, se presente, informa o esquema de tiles antes de qualquer `IDAT`, permitindo ao cliente montar um placeholder da imagem completa.
3. Cada `IDAT` é auto-contido e pode ser decodificado assim que chega, sem esperar os demais — exceto quando um `zDIC` está presente, caso em que o dicionário (lido antes do primeiro `IDAT`) precisa estar disponível ao processar qualquer `IDAT` subsequente.
4. Recomenda-se `scan_order = 1` (Z-order) combinado com `Interlace = 1` para melhor experiência de carregamento progressivo (preview de baixa qualidade da imagem inteira, refinando ao longo do tempo).

### 6.1 Encode em streaming (`Encoder<W>`, implementação de referência, v1.6+)

Simétrico ao decode em streaming: um encoder pode escrever o `IHDR` e o `IDAT` de cada tile incrementalmente à medida que os tiles ficam disponíveis, em vez de exigir a imagem inteira em memória antes de produzir qualquer saída. O `Encoder<W: Write>` da implementação de referência suporta isso apenas para color types diretos (`cinza`/`RGB`/`cinza+alfa`/`RGBA` — não indexado, já que a quantização de paleta precisa ver todo pixel antes que um único índice possa ser emitido) e não suporta entrelaçamento Adam7/par-ímpar (que precisa dos pixels da imagem inteira para intercalar). Dois esquemas de tiling são suportados:

- **Tiling em faixas de linhas** (padrão): o chamador submete faixas horizontais de altura arbitrária via `add_tile()`, um `IDAT` por chamada.
- **Tiling 2D (`iDIM`, desde v1.10)**: o chamador opta por isso fornecendo `tile_width`/`tile_height`/`scan_order` antes de escrever qualquer tile — diferente do modo em faixas de linhas, isso exige conhecer a geometria completa da grade de tiles antecipadamente (para que o `iDIM` possa ser escrito imediatamente após o `IHDR`, conforme a ordem obrigatória de chunks da seção 9), mas não os dados de pixel em si, permanecendo totalmente transmissível. Os tiles são submetidos um de cada vez via `add_idim_tile()`, na ordem de grade pré-computada (row-major ou Z-order, seção 5.2), cada um dimensionado exatamente para `tile_width × tile_height` (mais estreito/curto nas bordas direita/inferior quando `width`/`height` não são múltiplos exatos do tamanho de tile declarado). Os dois métodos de submissão (`add_tile()` e `add_idim_tile()`) são mutuamente exclusivos por instância de encoder.

Existem duas variantes, diferindo apenas em quão precisamente conseguem preencher o campo `Compression method` do `IHDR` (seção 4.1) depois de já ter sido escrito na saída:

- **Apenas `W: Write`** (ex: um socket bruto, ou qualquer destino que não permite seek para trás): o bit ZSTD do `Compression method` é ativado incondicionalmente, antes mesmo de qualquer tile ser comprimido — uma superestimativa na direção segura conforme a regra do limite-inferior-obrigatório da seção 4.1 (declarar `bit0 = 1` é sempre uma declaração de capacidade válida, independentemente de algum chunk de fato precisar disso). O resto do `IHDR` (dimensões, bit depth, sample format, color type, filter method) é exato desde o início, já que nenhum desses campos depende do conteúdo de chunks posteriores.
- **`W: Write + Seek`** (ex: um arquivo local, ou um buffer em memória): assim que o último tile é submetido, o encoder faz seek de volta e corrige o `Compression method` (recalculando o CRC32 do `IHDR`) para seu valor exato — byte-a-byte idêntico ao que o caminho de encode de arquivo completo teria produzido para os mesmos pixels e opções.

Como o paralelismo por tile (usado internamente ao comprimir uma imagem já totalmente em memória) exige conhecer antecipadamente o trabalho independente de cada tile, um encoder em streaming que recebe tiles um de cada vez conforme o chamador os produz necessariamente os comprime sequencialmente.

---

## 7. Extensibilidade HDR (implementada na referência desde v1.0)

Suporte ao fluxo HDR completo na implementação de referência, ao nível de formato:

- `Sample format` no `IHDR` (uint / float / half-float) — encode via `EncodeOptions.sample_format` / `--sample-format`; decode converte float/half de volta para RGBA (seção 4.1)
- Chunk `cHDR` como ancilar (seção 4.4) — implementado desde v1.0; decoders antigos ignoram e continuam funcionando
- Chunk `iCCP` (seção 4.7), também implementado, cobre parte do gerenciamento de cor

Um decoder sem suporte a HDR que encontra `cHDR` deve ignorá-lo (chunk ancilar) e, se `Sample format` ou `Bit depth` forem incompatíveis com seu suporte, deve rejeitar o arquivo de forma explícita — não deve tentar interpretar dados float como inteiro. Nesse caso, e em qualquer arquivo sem `cHDR`/`iCCP`, aplica-se a regra de espaço de cor padrão da seção 4.4: as cores devem ser interpretadas como sRGB.

**Tone mapping (implementado no decode da referência desde v1.1, fora do contrato binário):** quando o decoder encontra `Sample format = 1` (float) **e** um chunk `cHDR`, a conversão final para RGBA 8-bit aplica um pipeline de exibição SDR: (1) EOTF — `transfer_function` do `cHDR` (`0`=linear, `1`=PQ, `2`=HLG, `3`=sRGB) converte os valores codificados para luminância linear; (2) conversão de color primaries via CIE 1931 XYZ entre o espaço de origem do `cHDR` (`0`=BT.709/sRGB, `1`=BT.2020, `2`=DCI-P3) e o espaço de destino; (3) operador de tone mapping global (curva filmica ACES na referência, com Reinhard disponível) que comprime a faixa dinâmica para [0, 1]; (4) companding sRGB. Valores não-finitos (NaN/Inf), `max_luminance` degenerado e primaries/transfer inválidas são tratados de forma defensiva (clamp/erro tratável) — nunca geram panic. Este pipeline é uma **preferência de exibição do decoder**, não parte do contrato binário: um decoder pode ignorá-lo e apenas reduzir float→8-bit, e o resultado pode diferir visualmente sem quebrar a interoperabilidade. **Trabalho futuro:** encode SDR→HDR (tone mapping inverso), seleção de operador via CLI, e look-up tables.

---

## 8. Resumo de campos do IHDR (referência rápida)

| Byte # | Campo | Valores |
|---|---|---|
| 0-3 | Width | uint32 BE |
| 4-7 | Height | uint32 BE |
| 8 | Bit depth | 1, 2, 4, 8, 10, 12, 16, 32 — sub-byte (1, 2, 4) válidos apenas com Color types `0`, `3` e `4`; Color types `2` e `6` aceitam apenas `8, 10, 12, 16, 32` (ver seções 4.1.1 e 4.1.2) |
| 9 | Sample format | 0=uint, 1=float, 2=half-float |
| 10 | Color type | 0, 2, 3, 4, 6 (padrão: 6 = RGBA; 3 requer PLTE) |
| 11 | Compression method | bitmask: bit0=ZSTD, demais bits reservados para algoritmos futuros — ver a nota "Semântica precisa" da seção 4.1: isto é uma declaração de capacidade (limite inferior obrigatório dos codecs necessários), não um registro por chunk; a descompressão de cada chunk é sempre orientada pelo próprio byte `Flag` daquele chunk, nunca por este campo |
| 12 | Filter method | `0`=nenhum, `1`=byte-shuffle (seção 4.3.2, implementado), `2`=preditivo (código por bloco, seção 4.3.1) |
| 13 | Interlace method | 0=nenhum, 1=Adam7, 2=par/ímpar |

**Total: 14 bytes de payload no IHDR.**

---

## 9. Ordem obrigatória dos chunks

```
Assinatura (9 bytes)
IHDR                  (obrigatório, primeiro)
iDIM                  (opcional)
cHDR                  (opcional, instância única)
eXIF                  (opcional, instância única)
jSON (zero ou mais)   (opcional, um por namespace)
iCCP                  (opcional, instância única)
xMPd                  (opcional, instância única)
zDIC                  (opcional, instância única — deve preceder todo IDAT que dependa dele)
PLTE                  (obrigatório se Color type = 3, antes do primeiro IDAT)
IDAT (um ou mais)     (obrigatório, na ordem de escrita/leitura — ver seção 4.2 para tiling 2D)
IEND                  (obrigatório, último)
```

---

## 10. Considerações de design

- **Fallback por chunk** (não por arquivo inteiro) evita overhead de compressão em blocos de alta entropia (ruído, dados já comprimidos com perda).
- **CRC por chunk** permite detectar corrupção sem descomprimir o arquivo inteiro.
- **Convenção crítico/ancilar** permite adicionar novos chunks (ex: `iCCP`, `xMPd`, um futuro chunk de anotações geométricas) sem quebrar decoders antigos.
- Tamanho de tile é um trade-off: tiles menores = streaming mais granular, porém mais overhead de cabeçalho/CRC por chunk **e** menor eficiência do filtro preditivo (seção 4.3.1), já que cada tile reinicia a predição na primeira linha. Empiricamente (encoder de referência, auditoria v1.5), o tamanho comprimido melhora monotonicamente à medida que o tile de linhas cresce — até e incluindo "sem tiling algum" — em todo tipo de conteúdo testado (suave, alta frequência, foto-realista e conteúdo misto/transicional), sem reversão em nenhum tamanho de tile testado. Entretanto, o encoder de referência paraleliza a compressão de tiles em um pool de threads, então o **tempo de encode** (wall-clock) segue a curva oposta, em formato de U: tiles pequenos demais adicionam overhead de agendamento/framing por tile, enquanto tiles grandes demais deixam trabalho paralelo insuficiente para uma máquina multi-core, fazendo com que cada chamada ZSTD grande rode majoritariamente serial. O tamanho padrão de tile de linhas do encoder de referência (64 linhas) fica no ou perto do mínimo empírico dessa curva de tempo, tanto em máquinas com muitos núcleos quanto com poucos, trocando aproximadamente 5-15% de tamanho comprimido (vs. tiles muito maiores) por uma melhoria de 5-10x no tempo de encode — esta é uma decisão de ajuste do lado do encoder, não um requisito de formato; outros encoders podem escolher padrões diferentes ou expor o tamanho de tile como opção configurável pelo usuário.
- **Filtro preditivo não deve ser confundido com compressão**: ele reduz a entropia dos dados antes do ZSTD atuar, mas não substitui a etapa de compressão nem a regra de fallback da seção 3.2 — as duas técnicas atuam em conjunto.

---

## 11. Licenciamento

- **Texto desta especificação**: © 2026 Daniel Secco. Licenciado sob [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/) — qualquer pessoa pode implementar o formato CAFE livremente, inclusive comercialmente, desde que dê crédito ao autor original (Daniel Secco).
- **Implementações de referência** (código): **BSD-3-Clause** — permissivo, permite uso comercial, sem requisitos de copyleft.

---

## 12. Considerações de segurança

Um decoder CAFE processa dados de origem **não confiável** por definição (arquivos de terceiros, downloads, uploads de usuário). Esta seção documenta os requisitos mínimos que qualquer implementação deve seguir para não expor a aplicação hospedeira a negação de serviço (DoS) ou pior.

### 12.1 Princípio geral: decoders nunca devem gerar panic/crash em entrada não confiável

Todo campo de tamanho, contagem ou offset lido de um arquivo `.cafe` é, por definição, controlado por quem criou o arquivo — inclusive um atacante. Um decoder correto **valida esses campos antes de usá-los** para indexar memória, alocar buffers ou dividir valores, e retorna um erro tratável (não uma exceção não capturada/panic/segfault) para qualquer entrada malformada. Isso vale inclusive para:

- Arquivos truncados (menores que o esperado em qualquer ponto do parsing).
- Campos `Length` forjados (maiores que os dados realmente disponíveis, ou grandes o suficiente para causar overflow aritmético ao somar com offsets).
- Chunks críticos (`IHDR`, `PLTE`, `IDAT`) com tamanho menor que o mínimo exigido pela spec.
- Dimensões degeneradas (`Width = 0` ou `Height = 0`), que não devem ser usadas para dividir nenhum outro valor sem checagem prévia.
- Inconsistência entre as dimensões declaradas no `IHDR` e a quantidade real de dados de pixel reconstruídos a partir dos `IDAT` — deve gerar erro explícito, não uma tentativa de construir uma imagem com buffer de tamanho errado.

### 12.2 Proteção contra "decompression bomb" (CWE-409)

Como qualquer formato que empregue compressão genérica, o CAFE é vulnerável ao mesmo princípio de ataque de um "zip bomb": um chunk comprimido de poucos KB pode, em tese, se expandir para gigabytes ao ser descomprimido, esgotando toda a memória disponível do processo antes mesmo de qualquer validação de conteúdo acontecer.

**Requisito obrigatório**: todo decoder deve impor um **limite superior configurável** ao tamanho de saída de qualquer operação de descompressão (por chunk), rejeitando a descompressão assim que esse limite for excedido — sem jamais tentar alocar ou materializar o conteúdo completo além do limite antes de checar. A implementação de referência usa **1 GiB por chunk** como valor padrão, generoso o suficiente para imagens realistas em altíssima resolução, mas finito.

Isso é independente de (e complementar a) qualquer limite de tamanho do próprio arquivo `.cafe` — o objetivo aqui é especificamente sobre a **razão de expansão** da descompressão, que pode ser arbitrariamente alta mesmo em arquivos pequenos.

### 12.3 Ausência de limite superior para `Width`/`Height`

A spec não define um limite máximo para `Width`/`Height` no `IHDR`. Isso é intencional (evita impor um teto arbitrário que poderia invalidar casos de uso legítimos), mas implica que decoders devem tratar esses valores como não-confiáveis para fins de pré-alocação: a reconstrução da imagem deve ocorrer incrementalmente, a partir dos dados de pixel realmente recebidos nos `IDAT` (respeitando o limite da seção 12.2 por chunk), e não por meio de uma alocação única de `Width × Height × bytes_por_pixel` feita **antes** de qualquer dado real ter sido validado.

### 12.4 Chunks ancilares malformados nunca invalidam o arquivo nem geram panic

Reforçando a convenção da seção 3.1: um chunk ancilar (`eXIF`, `jSON`, `iCCP`, `xMPd`, `zDIC`, `cHDR`) com conteúdo malformado — namespace inconsistente, JSON inválido, XML inválido, UTF-8 inválido — deve ser **descartado silenciosamente** pelo decoder (ou reportado como aviso não-fatal), nunca deve interromper a decodificação da imagem em si, e nunca deve causar panic. A única exceção é quando o próprio chunk é estruturalmente impossível de delimitar (ex: `Length` que ultrapassa o arquivo) — nesse caso, o erro é de framing (seção 12.1), não de conteúdo, e é tratado no nível do parser de chunks, não do parser específico daquele chunk.

### 12.5 Escopo desta auditoria

As diretrizes acima refletem uma auditoria de segurança realizada sobre a implementação de referência em Rust, cobrindo: leitura de chunk (framing), header `IHDR`, descompressão (com e sem dicionário `zDIC`), decodificação de paleta indexada, chunk `jSON`, montagem final da imagem, **byte-shuffle** (seção 4.3.2) e **tone mapping HDR** (seção 7). Todos os pontos foram validados com testes adversariais (arquivos truncados, campos forjados, dimensões degeneradas, decompression bomb real de ~1 GiB disfarçada em dezenas de KB, valores NaN/Inf, `max_luminance`/`bytes per pixel` degenerados) confirmando comportamento de erro tratável, sem panic, em todos os casos.

Requisitos específicos da auditoria sobre as adições v1.1:

- **Byte-shuffle**: validação rígida de `bpp ∈ {2, 4, 8, 16}` antes de qualquer indexação; overflow-proteção em `largura × altura × bpp`; verificação de tamanho exato do buffer (truncamento → erro tratável, nunca read out-of-bounds); derivação defensiva da altura do tile (`len / bytes_por_linha` com `bytes_por_linha` guardado contra zero).
- **Tone mapping**: divisões por zero evitadas (EOTF com denominador protegido; `max_luminance.max(1.0)`); NaN/Inf em canais tratados explicitamente via `is_finite()`; overflow em `width × height × 16` via `checked_mul`; validação de tamanho exato do buffer float; valores fora de [0, máx] clamped antes do operador.

---

---

## A. Notas de Performance (Implementação de Referência, v1.1+)

### Aceleração SIMD (AVX2)

A implementação de referência inclui otimização AVX2 SIMD opcional para os Filtros 1 (Sub), 2 (Up) e 3 (Average) em CPUs x86_64:

- **Filtro 1 (Sub)**: Processa 32 bytes por iteração SIMD (4-8x mais rápido que escalar)
- **Filtro 2 (Up)**: Processa 32 bytes por iteração SIMD (4-8x mais rápido que escalar)
- **Filtro 3 (Average)**: Versão escalar otimizada (bom desempenho de baseline)
- **Fallback**: Fallback escalar automático em CPUs sem AVX2, ou quando a feature SIMD está desativada

**Feature gate:** `cargo build --release` (SIMD ativado por padrão), ou `cargo build --release --no-default-features` (desativa SIMD para portabilidade)

**Compatibilidade:** Esta aceleração é transparente aos decoders — SIMD não afeta o formato binário nem a interoperabilidade, apenas a velocidade de codificação.

---

## B. Aceleração SIMD Agressiva (Implementação de Referência v1.2+)

### Cobertura de Vetorização Estendida

A versão 1.2 adiciona otimização AVX2 abrangente além de filtros, focando em hotspots críticos de codificação/decodificação:

#### Pack/Unpack de Amostras 1/2/4-bit (`src/simd_packing.rs`)
- **Pack 1-bit**: 256 pixels por iteração SIMD, **speedup 8-16x** vs escalar
- **Pack 2-bit**: 128 pixels por iteração SIMD, **speedup 7-10x** vs escalar
- **Pack 4-bit**: 64 pixels por iteração SIMD, **speedup 5-7x** vs escalar
- **Unpack**: Speedups simétricos via extração e shuffles AVX2
- **Caso de uso**: Codificação de paleta indexada (color_type=3, bit_depth 1-4), cinza sub-byte (color_type=0, bit_depth 1-4)

#### Expansão/Redução de Amostras (`src/simd_sample_conversion.rs`)
- **8 → 16-bit**: Unpack via unpacklo/unpackhi AVX2, escala via shifts, **speedup 4-6x**
- **8 → 32-bit float**: Unpack, converte para floats IEEE 754 (divisão por 255), **speedup 4-6x**
- **16/32 → 8-bit**: Shuffle, saturate, pack via AVX2, **speedup 4-6x**
- **Caso de uso**: Conversões de formato de amostra (uint ↔ float, uint ↔ half-float), reduzindo dados float de volta para 8-bit para saída RGBA final

#### Byte-Shuffle com Blocking (`src/shuffle.rs` + Filter Method=1)
- **Tamanho de bloco**: 1024 pixels (cache-friendly)
- **Melhoria**: Redução de 10-20% em largura de banda de memória vs byte-shuffling ingênuo
- **Caso de uso**: Amostras multi-byte (bpp ∈ {2,4,8,16}), imagens HDR com dados float/half-float

#### Otimização do Filtro 3 (Average) (Melhorado em v1.2)
- **Implementação melhorada**: AVX2 unpacklo/unpackhi para melhor pareamento de lanes
- **Speedup**: 4-6x mais rápido que versão escalar v1.1
- **Melhoria principal**: Evita divisões intermediárias; em vez disso, usa médias baseadas em shifts com aritmética AVX2

### Performance Geral (Benchmarked v1.2)

**Carga de trabalho típica mista (indexada 512×512 + amostras float 256×256 + RGBA 1024×512):**
- **Speedup geral**: 2.8–3.5x vs v1.1 (escalar)
- **Melhorias de tempo de codificação**:
  - Nível 1: ~1.5x mais rápido (dominado por pack)
  - Nível 19 (padrão): ~1.6x mais rápido (blend de filtro + pack)
  - Nível 22 (máximo): ~1.6x mais rápido (teste de compressão + overhead de filtro)

**Razões de compressão reais (v1.2 inalterado de v1.1):**
- **Padrão xadrez** (indexado, compressão alta): 11.4× menor que PNG
- **Imagem de gradiente** (suave, amigável a filtro): 9.3× menor que PNG
- **Ruído aleatório** (baixa entropia, ganho de filtro limitado): 5.5× menor que PNG

### Testes & Validação (v1.2)

- **203 testes totais**:
  - 197 testes unitários (correção de filtro, precisão pack/unpack, casos extremos de conversão de amostra, cobertura de color type)
  - 6 testes de integração roundtrip (PNG → CAFE → PNG com variações de dimensão/padrão: 4×4 minúsculo, 2048×256 largo, 256×2048 alto)
- **Zero TODOs/FIXMEs** no código da biblioteca
- **Clippy limpo**: Todos os lints passam no escopo da biblioteca
- **Testes de regressão**: Zero falhas, taxa de aprovação de 100%
- **Detecção de CPU**: Verificação automática de capacidade AVX2 com fallback escalar gracioso em CPUs sem AVX2 (sem penalidade de runtime)

### Compilação e Controle de Feature

```bash
# Padrão (SIMD ativado em x86_64)
cargo build --release

# Desativa SIMD para portabilidade ou debugging
cargo build --release --no-default-features

# Força SIMD em CPU compatível (se auto-detect de feature falhar)
RUSTFLAGS="-C target-feature=+avx2" cargo build --release
```

### Compatibilidade & Forward Compatibility

- **Formato inalterado**: v1.2 produz arquivos CAFE idênticos a v1.1 (sem mudanças quebra-compatibilidade)
- **Decoders não afetados**: Otimização SIMD é apenas encoder; decoders ganham apenas em reversal de filtro mais rápido
- **Compatível com versões anteriores**: Decoders v1.2 leem todos os arquivos v1.1 e v1.0 sem modificação
- **Extensibilidade futura**: Tamanhos de bloco e limites SIMD podem ser ajustados por imagem (via CLI ou padrões de biblioteca) sem mudanças de formato

---

*Fim da especificação v1.2 (atualizada em 10 de agosto de 2026: SIMD AVX2 agressivo para pack/unpack/sample-conversion, 203 testes abrangentes, benchmarks Criterion, zero TODOs/FIXMEs).*

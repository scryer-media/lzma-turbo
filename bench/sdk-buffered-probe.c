/* SDK memory-input/memory-output RSS comparator.
 * Build against the pinned C SDK; see docs/encoder-perf-0.9.1.md.
 * Usage: sdk-buffered 2 THREADS INPUT [RAW_OUTPUT]
 * TRACE_ALLOC=1 logs allocations for a separate diagnostic run.
 */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include "Lzma2Enc.h"
#include "Alloc.h"

static int trace_alloc;
static void *small_alloc(ISzAllocPtr p, size_t size) {
  (void)p;
  void *v = ISzAlloc_Alloc(&g_Alloc, size);
  if (trace_alloc) fprintf(stderr, "SDK small allocation: %zu bytes\n", size);
  return v;
}
static void small_free(ISzAllocPtr p, void *v) {
  (void)p;
  ISzAlloc_Free(&g_Alloc, v);
}
static void *big_alloc(ISzAllocPtr p, size_t size) {
  (void)p;
  void *v = ISzAlloc_Alloc(&g_BigAlloc, size);
  if (trace_alloc) fprintf(stderr, "SDK big allocation: %zu bytes\n", size);
  return v;
}
static void big_free(ISzAllocPtr p, void *v) {
  (void)p;
  ISzAlloc_Free(&g_BigAlloc, v);
}
static const ISzAlloc small = {small_alloc, small_free};
static const ISzAlloc big = {big_alloc, big_free};

int main(int argc, char **argv) {
  if (argc < 4) return 2;
  trace_alloc = getenv("TRACE_ALLOC") != NULL;
  FILE *f = fopen(argv[3], "rb");
  if (!f || fseek(f, 0, SEEK_END)) return 3;
  long length = ftell(f);
  if (length < 0 || fseek(f, 0, SEEK_SET)) return 3;
  size_t size = (size_t)length;
  if (size > SIZE_MAX - (size >> 10) - 16) return 4;
  size_t capacity = size + (size >> 10) + 16;
  Byte *input = malloc(size ? size : 1);
  Byte *output = malloc(capacity);
  if (!input || !output || fread(input, 1, size, f) != size) return 4;
  fclose(f);
  CLzma2EncProps props;
  Lzma2EncProps_Init(&props);
  props.lzmaProps.level = 5;
  props.lzmaProps.numThreads = atoi(argv[2]) > 1 ? 2 : 1;
  props.numTotalThreads = atoi(argv[2]);
  CLzma2EncHandle enc = Lzma2Enc_Create(&small, &big);
  if (!enc) return 4;
  SRes result = Lzma2Enc_SetProps(enc, &props);
  Lzma2Enc_SetDataSize(enc, size);
  size_t written = capacity;
  if (result == SZ_OK)
    result = Lzma2Enc_Encode2(enc, NULL, output, &written, NULL, input, size, NULL);
  if (result != SZ_OK) { fprintf(stderr, "SDK error %d\n", result); return 5; }
  printf("%zu\n", written);
  if (trace_alloc) fprintf(stderr, "API input=%zu output_capacity=%zu output_length=%zu\n", size, capacity, written);
  if (argc > 4) {
    f = fopen(argv[4], "wb");
    if (!f || fwrite(output, 1, written, f) != written || fclose(f)) return 6;
  }
  Lzma2Enc_Destroy(enc);
  free(output);
  free(input);
  return 0;
}

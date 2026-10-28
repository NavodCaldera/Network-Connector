/* Throwaway: native twin of the sha_chain WAT module (same algorithm, same stdin/stdout format).
   stdin: 8 bytes LE round count + 32 byte seed. stdout: hex digest + "\n". stderr: loop time in ms (QPC). */
#include <fcntl.h>
#include <io.h>
#include <stdint.h>
#include <stdio.h>
#include <windows.h>

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
static const uint32_t H0[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                               0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};

#define ROTR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))

static void compress(uint32_t m[8]) {
    uint32_t w[64], a, b, c, d, e, f, g, h, t1, t2;
    int i;
    for (i = 0; i < 8; i++) w[i] = m[i];
    w[8] = 0x80000000u;
    for (i = 9; i < 15; i++) w[i] = 0;
    w[15] = 256;
    for (i = 16; i < 64; i++) {
        uint32_t x = w[i - 15], y = w[i - 2];
        w[i] = w[i - 16] + (ROTR(x, 7) ^ ROTR(x, 18) ^ (x >> 3)) + w[i - 7] + (ROTR(y, 17) ^ ROTR(y, 19) ^ (y >> 10));
    }
    a = H0[0]; b = H0[1]; c = H0[2]; d = H0[3]; e = H0[4]; f = H0[5]; g = H0[6]; h = H0[7];
    for (i = 0; i < 64; i++) {
        t1 = h + (ROTR(e, 6) ^ ROTR(e, 11) ^ ROTR(e, 25)) + ((e & f) ^ (~e & g)) + K[i] + w[i];
        t2 = (ROTR(a, 2) ^ ROTR(a, 13) ^ ROTR(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        h = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
    }
    m[0] = H0[0] + a; m[1] = H0[1] + b; m[2] = H0[2] + c; m[3] = H0[3] + d;
    m[4] = H0[4] + e; m[5] = H0[5] + f; m[6] = H0[6] + g; m[7] = H0[7] + h;
}

int main(void) {
    unsigned char in[40];
    uint32_t m[8];
    uint64_t n;
    LARGE_INTEGER f, t0, t1;
    int i;
    _setmode(_fileno(stdin), _O_BINARY);
    if (fread(in, 1, 40, stdin) != 40) return 2;
    n = 0;
    for (i = 7; i >= 0; i--) n = (n << 8) | in[i];
    for (i = 0; i < 8; i++)
        m[i] = ((uint32_t)in[8 + 4 * i] << 24) | ((uint32_t)in[9 + 4 * i] << 16) | ((uint32_t)in[10 + 4 * i] << 8) | in[11 + 4 * i];
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&t0);
    while (n--) compress(m);
    QueryPerformanceCounter(&t1);
    for (i = 0; i < 8; i++) printf("%08x", m[i]);
    printf("\n");
    fprintf(stderr, "loop_ms %.3f\n", (double)(t1.QuadPart - t0.QuadPart) * 1000.0 / (double)f.QuadPart);
    return 0;
}

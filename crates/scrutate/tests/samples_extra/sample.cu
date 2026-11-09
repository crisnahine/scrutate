#include <cuda_runtime.h>

// Add two vectors on the device.
__global__ void vector_add(const float *a, const float *b, float *c, int n) {
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) c[i] = a[i] + b[i];
}

template <typename T>
T clamp_value(T v, T lo, T hi) {
    return v < lo ? lo : (v > hi ? hi : v);
}

int launch(int n) {
    vector_add<<<1, n>>>(nullptr, nullptr, nullptr, n);
    return clamp_value(n, 0, 1024);
}

/*
 * Пример analyzer-плагина для ByteForge 2000.
 * Считает энтропию Шеннона и долю нулевых байт в открытом файле.
 *
 * Сборка:
 *   Linux:   gcc -shared -fPIC example_entropy_plugin.c -o entropy_plugin.so -lm
 *   Windows: cl /LD example_entropy_plugin.c /Fe:entropy_plugin.dll
 */

#include "plugin_api.h"
#include <math.h>
#include <stdio.h>
#include <string.h>

static const PluginInfo INFO = {
    "Entropy Scanner",
    "Считает энтропию Шеннона и долю нулевых байт открытого файла",
    PLUGIN_TYPE_ANALYZER,
    "1.0.0"
};

const PluginInfo *plugin_get_info(void) {
    return &INFO;
}

int plugin_process(const PluginRequest *request, PluginResult *result) {
    if (!request || !result || !result->log_buffer) {
        return 1;
    }

    if (!request->data || request->length == 0) {
        snprintf(result->log_buffer, result->log_capacity, "Empty file, nothing to analyze");
        return 0;
    }

    size_t histogram[256] = {0};
    size_t zero_bytes = 0;

    for (size_t i = 0; i < request->length; i++) {
        unsigned char b = request->data[i];
        histogram[b]++;
        if (b == 0) {
            zero_bytes++;
        }
    }

    double entropy = 0.0;
    for (int i = 0; i < 256; i++) {
        if (histogram[i] == 0) continue;
        double p = (double)histogram[i] / (double)request->length;
        entropy -= p * log2(p);
    }

    double zero_ratio = (double)zero_bytes / (double)request->length * 100.0;

    snprintf(
        result->log_buffer,
        result->log_capacity,
        "Entropy: %.3f bits/byte | Zero bytes: %.2f%% | Verdict: %s",
        entropy,
        zero_ratio,
        entropy > 7.5 ? "likely compressed/encrypted" : "likely structured data"
    );

    result->out_written = 0;
    return 0;
}

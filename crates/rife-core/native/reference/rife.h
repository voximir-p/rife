#pragma once
#include "net.h"
class RIFE {
public:
    RIFE(int gpu, int threads, int padding);
    ~RIFE();
    int load(const char* param, const unsigned char* weights, size_t weights_len);
    int process(const ncnn::Mat& a, const ncnn::Mat& b, float timestep, ncnn::Mat& out) const;
private:
    int process_v4(const ncnn::Mat&, const ncnn::Mat&, float, ncnn::Mat&) const;
    int process_v4_cpu(const ncnn::Mat&, const ncnn::Mat&, float, ncnn::Mat&) const;
    ncnn::VulkanDevice* vkdev;
    ncnn::Net flownet;
    ncnn::Pipeline* rife_preproc;
    ncnn::Pipeline* rife_postproc;
    ncnn::Pipeline* rife_v4_timestep;
    int num_threads;
    int padding;
};

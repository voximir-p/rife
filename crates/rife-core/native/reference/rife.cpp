// Embedded RIFE v4 path adapted from the user-provided rife-ncnn-vulkan-ex.
// See native/README.md for provenance and safety changes.
#include "rife.h"
#include "../settings.h"
#include "native_layers.h"
#include "cancellation.h"
#include "datareader.h"
#include <algorithm>
#include <vector>
#include <cstring>
#include "rife_preproc.comp.hex.h"
#include "rife_postproc.comp.hex.h"
#include "rife_v4_timestep.comp.hex.h"
#include "rife_ops.h"
DEFINE_LAYER_CREATOR(Warp)

RIFE::RIFE(int gpuid, int threads, int alignment)
    : vkdev(gpuid == -1 ? 0 : ncnn::get_gpu_device(gpuid)),
      rife_preproc(0), rife_postproc(0), rife_v4_timestep(0),
      num_threads(threads), padding(alignment) {}
RIFE::~RIFE() {
    delete rife_preproc; delete rife_postproc; delete rife_v4_timestep;
}
int RIFE::process(const ncnn::Mat& a, const ncnn::Mat& b, float t, ncnn::Mat& out) const {
    return vkdev ? process_v4(a,b,t,out) : process_v4_cpu(a,b,t,out);
}
// Bounded reads prevent truncated weight files from being read past their buffer.
class WeightReader : public ncnn::DataReader {
public:
    WeightReader(const unsigned char* data, size_t size) : data(data), remaining(size), truncated(false) {}
    size_t read(void* out, size_t count) const override {
        if (rife_cancelled()) { truncated = true; return 0; }
        if (count > remaining) truncated = true;
        count = std::min(count, remaining);
        memcpy(out, data, count);
        data += count;
        remaining -= count;
        return count;
    }
    bool failed() const { return truncated; }
private:
    mutable bool truncated;
    mutable const unsigned char* data;
    mutable size_t remaining;
};

int RIFE::load(const char* param, const unsigned char* weights, size_t weights_len)
{
    if (rife_cancelled()) return -233;
    ncnn::Option opt;
    opt.num_threads = num_threads;
    opt.use_vulkan_compute = vkdev ? true : false;
    opt.use_fp16_packed = vkdev ? true : false;
    opt.use_fp16_storage = vkdev ? true : false;
    opt.use_fp16_arithmetic = false;
    opt.use_int8_storage = true;

    flownet.opt = opt;

    flownet.set_vulkan_device(vkdev);

    flownet.register_custom_layer(RIFE_WARP_LAYER_NAME, Warp_layer_creator);

    const unsigned char* cursor = reinterpret_cast<const unsigned char*>(param);
    ncnn::DataReaderFromMemory parameters(cursor);
    if (flownet.load_param(parameters) != 0) return -1;
    WeightReader reader(weights, weights_len);
    if (flownet.load_model(reader) != 0 || reader.failed()) return -2;
    if (rife_cancelled()) return -233;

    // initialize preprocess and postprocess pipeline
    if (vkdev)
    {
        std::vector<ncnn::vk_specialization_type> specializations(1);
        specializations[0].i = 0; // RGB8 on every platform.

        {
            static std::vector<uint32_t> spirv;
            static ncnn::Mutex lock;
            {
                ncnn::MutexLockGuard guard(lock);
                if (spirv.empty())
                {
                    compile_spirv_module(rife_preproc_comp_data, sizeof(rife_preproc_comp_data), opt, spirv);
                }
            }

            rife_preproc = new ncnn::Pipeline(vkdev);
            rife_preproc->set_optimal_local_size_xyz(8, 8, RIFE_RGB_CHANNELS);
            if (rife_preproc->create(spirv.data(), spirv.size() * 4, specializations) != 0) return -4;
        }

        {
            static std::vector<uint32_t> spirv;
            static ncnn::Mutex lock;
            {
                ncnn::MutexLockGuard guard(lock);
                if (spirv.empty())
                {
                    compile_spirv_module(rife_postproc_comp_data, sizeof(rife_postproc_comp_data), opt, spirv);
                }
            }

            rife_postproc = new ncnn::Pipeline(vkdev);
            rife_postproc->set_optimal_local_size_xyz(8, 8, RIFE_RGB_CHANNELS);
            if (rife_postproc->create(spirv.data(), spirv.size() * 4, specializations) != 0) return -4;
        }
    }

        if (vkdev)
        {
            static std::vector<uint32_t> spirv;
            static ncnn::Mutex lock;
            {
                ncnn::MutexLockGuard guard(lock);
                if (spirv.empty())
                {
                    compile_spirv_module(rife_v4_timestep_comp_data, sizeof(rife_v4_timestep_comp_data), opt, spirv);
                }
            }

            std::vector<ncnn::vk_specialization_type> specializations;

            rife_v4_timestep = new ncnn::Pipeline(vkdev);
            rife_v4_timestep->set_optimal_local_size_xyz(8, 8, 1);
            if (rife_v4_timestep->create(spirv.data(), spirv.size() * 4, specializations) != 0) return -4;
        }

    return 0;
}

int RIFE::process_v4(const ncnn::Mat& in0image, const ncnn::Mat& in1image, float timestep, ncnn::Mat& outimage) const
{
    if (!vkdev)
    {
        // cpu only
        return process_v4_cpu(in0image, in1image, timestep, outimage);
    }

    if (timestep == 0.f)
    {
        outimage = in0image;
        return 0;
    }

    if (timestep == 1.f)
    {
        outimage = in1image;
        return 0;
    }

    const unsigned char* pixel0data = (const unsigned char*)in0image.data;
    const unsigned char* pixel1data = (const unsigned char*)in1image.data;
    const int w = in0image.w;
    const int h = in0image.h;
    const int channels = RIFE_RGB_CHANNELS;//in0image.elempack;

    ncnn::VkAllocator* blob_vkallocator = vkdev->acquire_blob_allocator();
    ncnn::VkAllocator* staging_vkallocator = vkdev->acquire_staging_allocator();
    struct Allocators {
        ncnn::VulkanDevice* device;
        ncnn::VkAllocator* blob;
        ncnn::VkAllocator* staging;
        ~Allocators() { device->reclaim_blob_allocator(blob); device->reclaim_staging_allocator(staging); }
    } allocators = {vkdev, blob_vkallocator, staging_vkallocator};

    ncnn::Option opt = flownet.opt;
    opt.blob_vkallocator = blob_vkallocator;
    opt.workspace_vkallocator = blob_vkallocator;
    opt.staging_vkallocator = staging_vkallocator;

    // Graph-derived RIFE padding
    int w_padded, h_padded;

    w_padded = (w + padding - 1) / padding * padding;
    h_padded = (h + padding - 1) / padding * padding;

    const size_t in_out_tile_elemsize = opt.use_fp16_storage ? 2u : 4u;

    ncnn::Mat in0;
    ncnn::Mat in1;
    if (opt.use_fp16_storage && opt.use_int8_storage)
    {
        in0 = ncnn::Mat(w, h, (unsigned char*)pixel0data, (size_t)channels, 1);
        in1 = ncnn::Mat(w, h, (unsigned char*)pixel1data, (size_t)channels, 1);
    }
    else
    {
        in0 = ncnn::Mat::from_pixels(pixel0data, ncnn::Mat::PIXEL_RGB, w, h);
        if (in0.empty()) return -100;
        in1 = ncnn::Mat::from_pixels(pixel1data, ncnn::Mat::PIXEL_RGB, w, h);
        if (in1.empty()) return -100;

    }

    ncnn::VkCompute cmd(vkdev);

    // upload
    ncnn::VkMat in0_gpu;
    ncnn::VkMat in1_gpu;
    {
        cmd.record_clone(in0, in0_gpu, opt);
        if (in0_gpu.empty()) return -100;
        cmd.record_clone(in1, in1_gpu, opt);
        if (in1_gpu.empty()) return -100;
    }

    ncnn::VkMat out_gpu;

        // preproc
        ncnn::VkMat in0_gpu_padded;
        ncnn::VkMat in1_gpu_padded;
        ncnn::VkMat timestep_gpu_padded;
        {
            in0_gpu_padded.create(w_padded, h_padded, RIFE_RGB_CHANNELS, in_out_tile_elemsize, 1, blob_vkallocator);
            if (in0_gpu_padded.empty()) return -100;

            std::vector<ncnn::VkMat> bindings(2);
            bindings[0] = in0_gpu;
            bindings[1] = in0_gpu_padded;

            std::vector<ncnn::vk_constant_type> constants(6);
            constants[0].i = in0_gpu.w;
            constants[1].i = in0_gpu.h;
            constants[2].i = in0_gpu.cstep;
            constants[3].i = in0_gpu_padded.w;
            constants[4].i = in0_gpu_padded.h;
            constants[5].i = in0_gpu_padded.cstep;

            cmd.record_pipeline(rife_preproc, bindings, constants, in0_gpu_padded);
        }
        {
            in1_gpu_padded.create(w_padded, h_padded, RIFE_RGB_CHANNELS, in_out_tile_elemsize, 1, blob_vkallocator);
            if (in1_gpu_padded.empty()) return -100;

            std::vector<ncnn::VkMat> bindings(2);
            bindings[0] = in1_gpu;
            bindings[1] = in1_gpu_padded;

            std::vector<ncnn::vk_constant_type> constants(6);
            constants[0].i = in1_gpu.w;
            constants[1].i = in1_gpu.h;
            constants[2].i = in1_gpu.cstep;
            constants[3].i = in1_gpu_padded.w;
            constants[4].i = in1_gpu_padded.h;
            constants[5].i = in1_gpu_padded.cstep;

            cmd.record_pipeline(rife_preproc, bindings, constants, in1_gpu_padded);
        }
        {
            timestep_gpu_padded.create(w_padded, h_padded, 1, in_out_tile_elemsize, 1, blob_vkallocator);
            if (timestep_gpu_padded.empty()) return -100;

            std::vector<ncnn::VkMat> bindings(1);
            bindings[0] = timestep_gpu_padded;

            std::vector<ncnn::vk_constant_type> constants(4);
            constants[0].i = timestep_gpu_padded.w;
            constants[1].i = timestep_gpu_padded.h;
            constants[2].i = timestep_gpu_padded.cstep;
            constants[3].f = timestep;

            cmd.record_pipeline(rife_v4_timestep, bindings, constants, timestep_gpu_padded);
        }

        ncnn::VkMat out_gpu_padded;
        
            // flownet
            ncnn::Extractor ex = flownet.create_extractor();
            ex.set_blob_vkallocator(blob_vkallocator);
            ex.set_workspace_vkallocator(blob_vkallocator);
            ex.set_staging_vkallocator(staging_vkallocator);

            if (ex.input(RIFE_INPUT_BLOB_0, in0_gpu_padded) != 0) return -3;
            if (ex.input(RIFE_INPUT_BLOB_1, in1_gpu_padded) != 0) return -3;
            if (ex.input(RIFE_INPUT_BLOB_2, timestep_gpu_padded) != 0) return -3;
            if (ex.extract(RIFE_OUTPUT_BLOB, out_gpu_padded, cmd) != 0) return -3;
            if (out_gpu_padded.empty() || out_gpu_padded.w < w || out_gpu_padded.h < h || out_gpu_padded.c != RIFE_RGB_CHANNELS) return -6;

        if (opt.use_fp16_storage && opt.use_int8_storage)
        {
            out_gpu.create(w, h, (size_t)channels, 1, blob_vkallocator);
            if (out_gpu.empty()) return -100;
        }
        else
        {
            out_gpu.create(w, h, channels, (size_t)4u, 1, blob_vkallocator);
            if (out_gpu.empty()) return -100;
        }

        // postproc
        {
            std::vector<ncnn::VkMat> bindings(2);
            bindings[0] = out_gpu_padded;
            bindings[1] = out_gpu;

            std::vector<ncnn::vk_constant_type> constants(6);
            constants[0].i = out_gpu_padded.w;
            constants[1].i = out_gpu_padded.h;
            constants[2].i = out_gpu_padded.cstep;
            constants[3].i = out_gpu.w;
            constants[4].i = out_gpu.h;
            constants[5].i = out_gpu.cstep;

            cmd.record_pipeline(rife_postproc, bindings, constants, out_gpu);
        }

    // download
    {
        ncnn::Mat out;

        if (opt.use_fp16_storage && opt.use_int8_storage)
        {
            out = ncnn::Mat(out_gpu.w, out_gpu.h, (unsigned char*)outimage.data, (size_t)channels, 1);
        }

        cmd.record_clone(out_gpu, out, opt);

        if (cmd.submit_and_wait() != 0) return -5;

        if (out.empty()) return -100;
        if (!(opt.use_fp16_storage && opt.use_int8_storage))
        {
            out.to_pixels((unsigned char*)outimage.data, ncnn::Mat::PIXEL_RGB);

        }
    }

    return 0;
}

int RIFE::process_v4_cpu(const ncnn::Mat& in0image, const ncnn::Mat& in1image, float timestep, ncnn::Mat& outimage) const
{
    if (timestep == 0.f)
    {
        outimage = in0image;
        return 0;
    }

    if (timestep == 1.f)
    {
        outimage = in1image;
        return 0;
    }

    const unsigned char* pixel0data = (const unsigned char*)in0image.data;
    const unsigned char* pixel1data = (const unsigned char*)in1image.data;
    const int w = in0image.w;
    const int h = in0image.h;
    const int channels = RIFE_RGB_CHANNELS;//in0image.elempack;

    ncnn::Option opt = flownet.opt;

    // Graph-derived RIFE padding
    int w_padded, h_padded;

    w_padded = (w + padding - 1) / padding * padding;
    h_padded = (h + padding - 1) / padding * padding;

    ncnn::Mat in0;
    ncnn::Mat in1;
    {
        in0 = ncnn::Mat::from_pixels(pixel0data, ncnn::Mat::PIXEL_RGB, w, h);
        if (in0.empty()) return -100;
        in1 = ncnn::Mat::from_pixels(pixel1data, ncnn::Mat::PIXEL_RGB, w, h);
        if (in1.empty()) return -100;

    }

    ncnn::Mat out;

        // preproc and border padding
        ncnn::Mat in0_padded;
        ncnn::Mat in1_padded;
        ncnn::Mat timestep_padded;
        {
            in0_padded.create(w_padded, h_padded, RIFE_RGB_CHANNELS);
            if (in0_padded.empty()) return -100;
            for (int q = 0; q < RIFE_RGB_CHANNELS; q++)
            {
                float* outptr = in0_padded.channel(q);

                int i = 0;
                for (; i < h; i++)
                {
                    const float* ptr = in0.channel(q).row(i);

                    int j = 0;
                    for (; j < w; j++)
                    {
                        *outptr++ = *ptr++ * (1 / 255.f);
                    }
                    for (; j < w_padded; j++)
                    {
                        *outptr++ = 0.f;
                    }
                }
                for (; i < h_padded; i++)
                {
                    for (int j = 0; j < w_padded; j++)
                    {
                        *outptr++ = 0.f;
                    }
                }
            }
        }
        {
            in1_padded.create(w_padded, h_padded, RIFE_RGB_CHANNELS);
            if (in1_padded.empty()) return -100;
            for (int q = 0; q < RIFE_RGB_CHANNELS; q++)
            {
                float* outptr = in1_padded.channel(q);

                int i = 0;
                for (; i < h; i++)
                {
                    const float* ptr = in1.channel(q).row(i);

                    int j = 0;
                    for (; j < w; j++)
                    {
                        *outptr++ = *ptr++ * (1 / 255.f);
                    }
                    for (; j < w_padded; j++)
                    {
                        *outptr++ = 0.f;
                    }
                }
                for (; i < h_padded; i++)
                {
                    for (int j = 0; j < w_padded; j++)
                    {
                        *outptr++ = 0.f;
                    }
                }
            }
        }
        {
            timestep_padded.create(w_padded, h_padded, 1);
            if (timestep_padded.empty()) return -100;
            timestep_padded.fill(timestep);
        }

        ncnn::Mat out_padded;
        
            // flownet
            ncnn::Extractor ex = flownet.create_extractor();

            if (ex.input(RIFE_INPUT_BLOB_0, in0_padded) != 0) return -3;
            if (ex.input(RIFE_INPUT_BLOB_1, in1_padded) != 0) return -3;
            if (ex.input(RIFE_INPUT_BLOB_2, timestep_padded) != 0) return -3;
            if (ex.extract(RIFE_OUTPUT_BLOB, out_padded) != 0) return -3;
            if (out_padded.empty() || out_padded.w < w || out_padded.h < h || out_padded.c != RIFE_RGB_CHANNELS) return -6;

        // cut padding and postproc
        out.create(w, h, RIFE_RGB_CHANNELS);
        if (out.empty()) return -100;
        
            for (int q = 0; q < RIFE_RGB_CHANNELS; q++)
            {
                float* outptr = out.channel(q);
                for (int i = 0; i < h; i++)
                {
                    const float* ptr = out_padded.channel(q).row(i);
                    for (int j = 0; j < w; j++)
                    {
                        *outptr++ = *ptr++ * 255.f + 0.5f;
                    }
                }
            }

    // download
    {
        out.to_pixels((unsigned char*)outimage.data, ncnn::Mat::PIXEL_RGB);

    }

    return 0;
}

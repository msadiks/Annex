// Annex receiver shaders. Compiled at startup with D3DCompile (d3dcompiler_47
// ships with Windows 10 and 11).
//
// Colour: BT.709, full range, 8-bit (spec 3.6).

cbuffer Params : register(b0)
{
    float2 uv_scale;   // decoded picture size / decoder texture size
    float2 src_size;   // bicubic: source size in texels
    float4 rect;       // quad: left, top, right, bottom in clip space
};

Texture2D<float>  texY   : register(t0);
Texture2D<float2> texUV  : register(t1);
Texture2D<float4> texRGB : register(t2);
SamplerState linearClamp : register(s0);
SamplerState pointClamp  : register(s1);

struct VSOut
{
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

// One triangle covering the viewport.
VSOut vs_full(uint id : SV_VertexID)
{
    VSOut o;
    float2 p = float2((id << 1) & 2, id & 2);
    o.uv = p;
    o.pos = float4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    return o;
}

// Four-vertex strip covering `rect`.
VSOut vs_quad(uint id : SV_VertexID)
{
    VSOut o;
    float2 p = float2(id & 1, id >> 1);
    o.uv = p;
    o.pos = float4(lerp(rect.x, rect.z, p.x), lerp(rect.y, rect.w, p.y), 0.0, 1.0);
    return o;
}

float3 ycc_to_rgb(float y, float2 c)
{
    c -= 0.5;
    float r = y + 1.5748 * c.y;
    float b = y + 1.8556 * c.x;
    float g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    return saturate(float3(r, g, b));
}

// Fast: native 4:2:0, chroma bilinear (sited at the 2x2 block centre).
float4 ps_fast(VSOut i) : SV_Target
{
    float2 uv = i.uv * uv_scale;
    return float4(ycc_to_rgb(texY.Sample(linearClamp, uv), texUV.Sample(linearClamp, uv)), 1.0);
}

// Sharp: the decoded frame is 2W x 2H. At an output pixel centre the luma UV
// lands exactly between the four copies of that pixel's luma, so the bilinear
// sample is their mean (which also averages codec noise). The chroma plane is
// W x H, one sample per output pixel: point sample it.
float4 ps_sharp(VSOut i) : SV_Target
{
    float2 uv = i.uv * uv_scale;
    return float4(ycc_to_rgb(texY.Sample(linearClamp, uv), texUV.Sample(pointClamp, uv)), 1.0);
}

// Catmull-Rom bicubic upscale of an RGB texture.
float4 ps_bicubic(VSOut i) : SV_Target
{
    float2 pos = i.uv * src_size - 0.5;
    float2 f = frac(pos);
    int2 base = int2(floor(pos));
    float2 w[4];
    w[0] = f * (-0.5 + f * (1.0 - 0.5 * f));
    w[1] = 1.0 + f * f * (-2.5 + 1.5 * f);
    w[2] = f * (0.5 + f * (2.0 - 1.5 * f));
    w[3] = f * f * (-0.5 + 0.5 * f);
    int2 maxc = int2(src_size) - 1;
    float3 sum = 0;
    [unroll] for (int y = 0; y < 4; y++)
    {
        [unroll] for (int x = 0; x < 4; x++)
        {
            int2 c = clamp(base + int2(x - 1, y - 1), int2(0, 0), maxc);
            sum += texRGB.Load(int3(c, 0)).rgb * (w[x].x * w[y].y);
        }
    }
    return float4(saturate(sum), 1.0);
}

// Premultiplied texture (overlay, cursor).
float4 ps_tex(VSOut i) : SV_Target
{
    return texRGB.Sample(linearClamp, i.uv);
}

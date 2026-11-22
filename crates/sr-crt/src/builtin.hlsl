// Built-in passes of the CRT runtime.
//
// Port of ShaderGlass's preprocess shader (PreprocessShaderDef.h: the input copied into the
// chain's Original texture, flipped by the MVP, turned for the vertical orientation by
// swapping x and y) and passthrough ("none"). ShaderGlass, Copyright (C) 2021-2025 mausimus
// (mausimus.net), https://github.com/mausimus/ShaderGlass, GNU General Public License v3.0.
//
// Added here: global_Gamma, the game's gamma (the picture the player sees without CRT has
// it applied by the present, `ps_blit`; ShaderGlass captures a picture that already has it).

cbuffer UBO : register(b0)
{
    row_major float4x4 global_MVP : packoffset(c0);
    float global_SGVertical : packoffset(c4);
    float global_Gamma : packoffset(c4.y);
};

Texture2D<float4> Source : register(t2);
SamplerState _Source_sampler : register(s2);

struct VsIn
{
    float4 Position : TEXCOORD0;
    float2 TexCoord : TEXCOORD1;
};

struct VsOut
{
    float2 vTexCoord : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

VsOut vs_main(VsIn i)
{
    VsOut o;
    float4 p = mul(i.Position, global_MVP);
    o.gl_Position = global_SGVertical != 0 ? p.yxzw : p;
    o.vTexCoord = i.TexCoord;
    return o;
}

float4 ps_main(VsOut i) : SV_Target0
{
    float4 c = Source.Sample(_Source_sampler, i.vTexCoord);
    return float4(pow(saturate(c.rgb), global_Gamma), 1);
}

float4 ps_copy(VsOut i) : SV_Target0
{
    return Source.Sample(_Source_sampler, i.vTexCoord);
}

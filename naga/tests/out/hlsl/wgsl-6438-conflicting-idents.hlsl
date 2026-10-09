struct OurVertexShaderOutput {
    float4 position : SV_Position;
    float2 texcoord : LOC0;
};

struct VertexOutput_vs {
    float2 texcoord : LOC0;
    float4 position : SV_Position;
};

VertexOutput_vs vs(float2 xy : LOC0)
{
    OurVertexShaderOutput vsOutput = (OurVertexShaderOutput)0;

    vsOutput.position = float4(xy, 0.0f, 1.0f);
    OurVertexShaderOutput _e6 = vsOutput;
    const OurVertexShaderOutput ourvertexshaderoutput = _e6;
    const VertexOutput_vs ourvertexshaderoutput_1 = { ourvertexshaderoutput.texcoord, ourvertexshaderoutput.position };
    return ourvertexshaderoutput_1;
}

float4 fs() : SV_Target0
{
    return float4(1.0f, 0.0f, 0.0f, 1.0f);
}

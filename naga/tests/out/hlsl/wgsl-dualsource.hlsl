struct FragmentOutput {
    float4 output0_ : SV_Target0;
    float4 output1_ : SV_Target1;
};

FragmentOutput ConstructFragmentOutput(float4 arg0, float4 arg1) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.output0_ = arg0;
    ret.output1_ = arg1;
    return ret;
}

FragmentOutput main()
{
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(float4(0.4f, 0.3f, 0.2f, 0.1f), float4(0.9f, 0.8f, 0.7f, 0.6f));
    return fragmentoutput;
}

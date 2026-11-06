#include "../gxm_backend.c"
static unsigned checks,failures;
#define EXPECT(x) do{++checks;if(!(x)){++failures;fprintf(stderr,"FAIL %d %s\n",__LINE__,#x);}}while(0)
int main(void){
 for(unsigned id=88;id<105;++id){
  KvEffectVertex v[3]={0};float p[8]={(float)material_operation(id),0,960,540,-1,0,0,0};
  if(id<96){p[1]=(id%2==0);p[6]=(id%2!=0);}
  EXPECT(effect_inputs(v,3,id,5,p)==0);
  for(unsigned op=1;op<=24;++op){float q[8];memcpy(q,p,sizeof(q));q[0]=op;
   EXPECT((effect_inputs(v,3,id,5,q)==0)==(op==material_operation(id)));}
  for(unsigned field=0;field<8;++field){float q[8];memcpy(q,p,sizeof(q));q[field]=NAN;EXPECT(effect_inputs(v,3,id,5,q)==KV_BAD_ARGUMENT);}
  for(unsigned field=0;field<8;++field){KvEffectVertex w[3];memcpy(w,v,sizeof(w));
   for(unsigned k=0;k<3;++k) w[k].haze[field/4][field%4]=.75f;
   EXPECT((effect_inputs(w,3,id,5,p)==0)==!!material_haze(id));}
  for(unsigned field=0;field<12;++field){KvEffectVertex w[3];memcpy(w,v,sizeof(w));
   w[0].soft[field/4][field%4]=.25f;EXPECT(effect_inputs(w,3,id,5,p)==KV_BAD_ARGUMENT);}
  float q[8];memcpy(q,p,sizeof(q));q[4]=0;q[5]=8;EXPECT(effect_inputs(v,3,id,5,q)==KV_BAD_ARGUMENT);
  memcpy(q,p,sizeof(q));q[7]=1;EXPECT(effect_inputs(v,3,id,5,q)==KV_BAD_ARGUMENT);
  if(id<96){memcpy(q,p,sizeof(q));q[1]=1-q[1];EXPECT(effect_inputs(v,3,id,5,q)==KV_BAD_ARGUMENT);
   memcpy(q,p,sizeof(q));q[6]=1-q[6];EXPECT(effect_inputs(v,3,id,5,q)==KV_BAD_ARGUMENT);}
  EXPECT(!effect_candidate_enabled(id,UINT64_MAX & ~KV_OPT_PS_MATERIAL_VARIANTS));
  EXPECT(effect_candidate_enabled(id,UINT64_MAX));
  if(material_half(id)) EXPECT(!effect_candidate_enabled(id,UINT64_MAX & ~KV_OPT_HALF_COLOR));
  if(id==92 || id==93) EXPECT(!effect_candidate_enabled(id,UINT64_MAX & ~KV_OPT_PS_PLAIN_OP7));
 }
 printf("RESULT %s: %u checks, %u failures; actual material C input/options\n",failures?"FAIL":"PASS",checks,failures);
 return failures?1:0;
}

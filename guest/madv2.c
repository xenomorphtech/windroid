#include <stdio.h>
#include <sys/mman.h>
#include <time.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
int main(void){
  double t0=now(); char *p=mmap(0,0xc00000000ul,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE,-1,0); double t1=now();
  printf("mmap 48G RW: %p %.1f ms\n",(void*)p,(t1-t0)*1e3);
  if(p==MAP_FAILED) return 1;
  p[0x100000]=1; double t2=now();
  for(int i=0;i<5;i++) madvise(p+0x100000,4096,MADV_DONTNEED);
  double t3=now(); printf("touch %.3f ms, madvise x5 %.1f ms each\n",(t2-t1)*1e3,(t3-t2)*1e3/5);
  return 0;}

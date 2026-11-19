@echo off
rem Builds and runs the struct size / member offset probes with 32-bit MSVC (Visual Studio Build Tools).
rem Needs a ZoneTool checkout (GPL-3.0, https://github.com/ZoneTool/zonetool); its path is ZT in the two scripts.
rem   python gen_sizes.py                 -> iw4.cpp, codo.cpp   (sizeof of every top-level struct)
rem   python gen_offsets.py Material,XModel,...  -> off_iw4.cpp  (offsetof of every member)
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars32.bat" >nul 2>&1
cl /nologo /EHsc /std:c++17 /w iw4.cpp /Feiw4.exe
cl /nologo /EHsc /std:c++17 /w codo.cpp /Fecodo.exe
iw4.exe > iw4.sizes
codo.exe > codo.sizes

LaTeX package 'ifoddpage'
~~~~~~~~~~~~~~~~~~~~~~~~~
Copyright (c) 2011-2022 by Martin Scharrer <martin.scharrer@web.de>  
License: LaTeX Project Public License, v1.3 or later: http://www.latex-project.org/lppl.txt  
Repository: https://github.com/MartinScharrer/ifoddpage  
Issues: https://github.com/MartinScharrer/ifoddpage/issues  

This LaTeX package provides a \ifoddpage conditional to determine if the current page is odd or even.
The macro \checkoddpage must be used direct before to check the page number using a label.
Two compiler runs are therefore required to achieve correct results.
In addition the conditional \ifoddpageoronside is provided which is also true in oneside mode where 
all pages use the odd page layout.

This package got inspired from the 'changepage' package which provides (and uses) the same macros (\checkoddpage and
\ifoddpage) for its main function of changing the page design mid-way in the document.  Both packages can be used
together, but the 'ifoddpage' should be loaded last in this case, otherwise the \ifoddpageoroneside conditional will
stop working.

